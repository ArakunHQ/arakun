//! Subagents a session runs, each with a transcript of its own. Claude's
//! adapter, told the client takes `subagents`, runs each Agent call as a child
//! session: in the AIR form (`subagent_spawned`, `subagent_state_update`) when
//! the client also takes JetBrains AIR, else in the RFD's (`subagent_update`).
//! The child's own updates come on its session id and land in a read-only
//! [`Thread`] that the transcript view renders as it would any session.
//!
//! Agents without child sessions (OpenCode, Droid) report a subagent as a Task
//! call whose calls carry its id as their parent; those are mirrored into a
//! child thread too, so the sidebar lists every subagent the same way.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    AgentKind, Entry, Thread, ThreadEvent, ThreadStatus, ToolCall, ToolStatus, content_text,
    str_of, unix_now,
};
use crate::agent::subagent_trail::{self, Activity};

/// Where AIR's left-out Agent call is stood in for in the transcript that started the subagent.
const MARKER: &str = "subagent:";

/// How often the transcripts of running subagents are read.
const TRAIL_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SubagentState {
    pub fn is_running(self) -> bool {
        self == SubagentState::Running
    }

    pub fn label(self) -> &'static str {
        match self {
            SubagentState::Running => "Running",
            SubagentState::Completed => "Done",
            SubagentState::Failed => "Failed",
            SubagentState::Cancelled => "Stopped",
        }
    }

    /// AIR's terminal states: completed, failed, cancelled, disconnected.
    fn from_air(s: &str) -> Self {
        match s {
            "completed" => SubagentState::Completed,
            "failed" => SubagentState::Failed,
            _ => SubagentState::Cancelled,
        }
    }

    /// The RFD's work state: running or requires_action, else idle with a stop reason.
    fn from_rfd(state: &Value) -> Self {
        match state["state"].as_str() {
            Some("running" | "requires_action") => SubagentState::Running,
            _ => match state["stopReason"].as_str() {
                None | Some("end_turn") => SubagentState::Completed,
                Some("cancelled") => SubagentState::Cancelled,
                Some(_) => SubagentState::Failed,
            },
        }
    }

    fn of_call(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Pending | ToolStatus::InProgress => SubagentState::Running,
            ToolStatus::Completed => SubagentState::Completed,
            ToolStatus::Failed => SubagentState::Failed,
        }
    }

    fn call_status(self) -> ToolStatus {
        match self {
            SubagentState::Running => ToolStatus::InProgress,
            SubagentState::Completed => ToolStatus::Completed,
            SubagentState::Failed | SubagentState::Cancelled => ToolStatus::Failed,
        }
    }
}

/// A subagent as a saved transcript keeps it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub task: String,
    pub state: SubagentState,
    /// Unix seconds.
    pub started: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<u64>,
    /// The session that started it, when that is another subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The Agent call in the parent's transcript that started it, if any.
    /// Without it, a reopened transcript would mirror that call as a second subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The session ids of its earlier generations, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier: Vec<String>,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

pub struct Subagent {
    /// Its child session's id; for a mirrored one, its Task call's.
    pub id: String,
    pub name: String,
    pub task: String,
    pub state: SubagentState,
    pub started: u64,
    pub ended: Option<u64>,
    /// Made from the parent's Task call and the calls under it; rebuilt from
    /// the transcript, never saved on its own.
    pub mirrored: bool,
    /// The Agent call that started it, when the parent's transcript has one.
    pub call: Option<String>,
    /// The session ids of its earlier generations, oldest first: AIR runs a
    /// subagent that SendMessage resumes as a new child session.
    pub earlier: Vec<String>,
    host: Option<String>,
    pub thread: Entity<Thread>,
}

impl Subagent {
    /// How long it ran, or has been running.
    pub fn elapsed_secs(&self) -> u64 {
        self.ended
            .unwrap_or_else(unix_now)
            .saturating_sub(self.started)
    }

    /// Whether `id` names it: its own id, an earlier generation's, or the
    /// Agent call that started it.
    pub fn answers_to(&self, id: &str) -> bool {
        self.id == id || self.call.as_deref() == Some(id) || self.earlier.iter().any(|e| e == id)
    }
}

/// The agent a child session runs: AIR names a resumed one's session
/// `<agent>:generation:<n>`.
pub fn agent_of(session: &str) -> &str {
    session
        .split_once(":generation:")
        .map_or(session, |(agent, _)| agent)
}

impl ToolCall {
    /// The subagent an Agent call in the transcript opens: AIR's stand-in
    /// names its child session, a Task call stands for its own.
    pub fn subagent_id(&self) -> Option<&str> {
        match self.id.strip_prefix(MARKER) {
            Some(id) => Some(id),
            None => self.is_subagent().then_some(self.id.as_str()),
        }
    }
}

/// The Agent call AIR leaves out of the parent's transcript.
fn marker_call(id: &str, name: &str, task: &str) -> ToolCall {
    ToolCall {
        id: format!("{MARKER}{id}"),
        title: name.to_string(),
        kind: "other".into(),
        status: ToolStatus::InProgress,
        content: Vec::new(),
        locations: Vec::new(),
        raw_input: Some(json!({"description": name, "prompt": task})),
        raw_output: None,
        added: 0,
        removed: 0,
        terminal_output: String::new(),
        terminal_dropped_lines: 0,
        exit_code: None,
        parent: None,
        tool_name: Some("Agent".into()),
    }
}

fn text_of(v: &Value) -> Option<String> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(content_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => content_text(v),
        _ => return None,
    };
    Some(text).filter(|t| !t.trim().is_empty())
}

impl Thread {
    // ---- accessors -------------------------------------------------------

    /// The subagents this session ran, in the order they started.
    pub fn subagents(&self) -> &[Subagent] {
        &self.subagents
    }

    pub fn subagent(&self, id: &str) -> Option<&Subagent> {
        self.subagents.iter().find(|s| s.answers_to(id))
    }

    pub fn running_subagents(&self) -> usize {
        self.subagents
            .iter()
            .filter(|s| s.state.is_running())
            .count()
    }

    /// The user has the session back while the agent still works: subagents
    /// run in the background, the prompt they hold is unanswered, or the agent
    /// took a turn of its own once one finished.
    pub fn in_background(&self) -> bool {
        self.held || self.auto_turn || self.running_subagents() > 0
    }

    /// The turn that just ended left work going that the agent comes back
    /// to by itself: subagents, or a command it started in the background.
    /// Claude takes a turn of its own once that ends, so the work is not done.
    pub fn left_work_running(&self) -> bool {
        let turn = self.entries.get(self.turn_from..).unwrap_or_default();
        self.held || self.running_subagents() > 0 || background_commands(turn) > 0
    }

    /// A subagent's transcript, kept for it by the session that runs it.
    pub fn is_subagent_view(&self) -> bool {
        self.parent.is_some()
    }

    /// A running subagent's latest step, as its transcript file shows it.
    /// It shows the subagent works when its live updates don't come.
    pub fn trail(&self) -> Option<&Activity> {
        self.trail.as_ref().filter(|_| self.is_busy())
    }

    // ---- child threads ---------------------------------------------------

    /// A read-only thread for a subagent's transcript.
    fn child_thread(&self, name: &str, cx: &mut Context<Self>) -> Entity<Thread> {
        let parent = cx.weak_entity();
        let (kind, cwd) = (self.kind, self.cwd.clone());
        let mut meta = self.meta();
        meta.title = name.to_string();
        meta.session_id = None;
        cx.new(|_| {
            let mut t = Thread::new(kind, cwd, Some(meta));
            t.parent = Some(parent);
            t.status = ThreadStatus::Idle;
            t
        })
    }

    /// The prompt a subagent was given, as its transcript's first message.
    fn child_prompt(&mut self, text: String, cx: &mut Context<Self>) {
        let ix = self.push_entry(
            Entry::User {
                text,
                at: unix_now(),
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            cx,
        );
        self.turn_user = Some(ix);
    }

    /// A subagent's thread follows its state: working while it runs, its
    /// prompt stamped with how long it took once it ends.
    fn child_state(&mut self, state: SubagentState, cx: &mut Context<Self>) {
        if state.is_running() {
            self.turn_started.get_or_insert_with(Instant::now);
            if !self.is_busy() {
                self.set_status(ThreadStatus::Working, cx);
            }
            return;
        }
        let elapsed = self
            .turn_started
            .take()
            .map(|t| t.elapsed().as_millis() as u64);
        if let Some(ix) = self.turn_user
            && let Some(Entry::User { elapsed_ms, .. }) = self.entries.get_mut(ix)
            && elapsed.is_some()
        {
            *elapsed_ms = elapsed;
            self.entries_changed(ix, cx);
        }
        match state {
            SubagentState::Failed => self.notice("The subagent failed.", true, cx),
            SubagentState::Cancelled => self.notice("Stopped", false, cx),
            _ => {}
        }
        self.permissions.clear();
        self.native.questions.clear();
        self.trail = None;
        self.set_status(ThreadStatus::Idle, cx);
    }

    /// Working or idle as `state` is, without marking the transcript.
    fn follow_state(&mut self, state: SubagentState, cx: &mut Context<Self>) {
        if state.is_running() {
            self.turn_started.get_or_insert_with(Instant::now);
            self.set_status(ThreadStatus::Working, cx);
        } else {
            self.turn_started = None;
            self.set_status(ThreadStatus::Idle, cx);
        }
    }

    /// Takes a mirrored call as it is now: replaced in place, or appended.
    fn put_tool(&mut self, tool: ToolCall, cx: &mut Context<Self>) {
        match self.tool_index.get(&tool.id) {
            Some(&ix) => {
                self.entries[ix] = Entry::Tool(tool);
                self.entries_changed(ix, cx);
            }
            None => {
                self.push_entry(Entry::Tool(tool), cx);
            }
        }
    }

    /// Shows `entries` as the whole transcript, redrawing what changed.
    fn show_entries(&mut self, entries: Vec<Entry>, cx: &mut Context<Self>) {
        let status = self.status.clone();
        let mut t = self.transcript(cx);
        t.entries = entries;
        self.show_transcript(t, status, cx);
        self.turn_user = self
            .entries
            .iter()
            .position(|e| matches!(e, Entry::User { .. }));
    }

    // ---- updates from the agent ------------------------------------------

    /// An update on a session other than this one: a subagent's.
    pub(super) fn child_update(&mut self, sid: &str, u: &Value, cx: &mut Context<Self>) {
        if self.replaying && self.suppress_replay {
            return;
        }
        let kind = u["sessionUpdate"].as_str().unwrap_or("");
        if matches!(
            kind,
            "subagent_spawned" | "subagent_state_update" | "subagent_update"
        ) {
            return self.subagent_update(u, Some(sid), cx);
        }
        let Some(ix) = self.subagents.iter().position(|s| s.id == sid) else {
            return;
        };
        // Claude's adapter in the RFD form keeps the Agent call in the parent's
        // transcript, which mirrored the subagent already: the child session is it.
        if let Some(call) = str_of(&u["_meta"]["claudeCode"], "parentToolUseId")
            && self.subagents[ix].call.is_none()
            && let Some(m) = self
                .subagents
                .iter()
                .position(|s| s.mirrored && s.id == call)
        {
            self.subagents.remove(m);
            let ix = self
                .subagents
                .iter()
                .position(|s| s.id == sid)
                .unwrap_or(ix);
            self.subagents[ix].call = Some(call);
            cx.emit(ThreadEvent::SubagentsChanged { spawned: None });
        }
        let Some(child) = self
            .subagents
            .iter()
            .find(|s| s.id == sid)
            .map(|s| s.thread.clone())
        else {
            return;
        };
        child.update(cx, |c, cx| c.update(u, cx));
    }

    /// `subagent_spawned` and `subagent_state_update` (AIR), `subagent_update`
    /// (the RFD); `host` is the subagent whose session it came on, if any.
    pub(super) fn subagent_update(
        &mut self,
        u: &Value,
        host: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match u["sessionUpdate"].as_str() {
            Some("subagent_spawned") => {
                let Some(id) = str_of(u, "subagentSessionId") else {
                    return;
                };
                let task = str_of(u, "task").unwrap_or_default();
                let name = str_of(u, "name")
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "Subagent".into());
                let prompt = u
                    .get("prompt")
                    .and_then(text_of)
                    .or_else(|| Some(task.clone()).filter(|t| !t.trim().is_empty()));
                self.spawn_subagent(id, name, task, prompt, host, true, cx);
            }
            Some("subagent_state_update") => {
                if let Some(id) = str_of(u, "subagentSessionId") {
                    let state = SubagentState::from_air(u["state"].as_str().unwrap_or(""));
                    self.set_subagent_state(&id, state, cx);
                }
            }
            Some("subagent_update") => {
                let Some(id) = str_of(u, "sessionId") else {
                    return;
                };
                let title = str_of(u, "title").filter(|t| !t.trim().is_empty());
                let description = str_of(u, "description").filter(|t| !t.trim().is_empty());
                if !self.subagents.iter().any(|s| s.id == id) {
                    let name = title
                        .clone()
                        .or_else(|| description.clone())
                        .unwrap_or_else(|| "Subagent".into());
                    // The prompt comes as the child's first `session_message`.
                    self.spawn_subagent(
                        id.clone(),
                        name,
                        description.unwrap_or_default(),
                        None,
                        host,
                        false,
                        cx,
                    );
                } else if let Some(title) = title
                    && let Some(s) = self.subagents.iter_mut().find(|s| s.id == id)
                {
                    s.name = title;
                }
                if u["state"].is_object() {
                    self.set_subagent_state(&id, SubagentState::from_rfd(&u["state"]), cx);
                }
            }
            _ => {}
        }
    }

    /// The RFD's `session_message` on a subagent's session: what it was asked.
    pub(super) fn session_message(&mut self, u: &Value, cx: &mut Context<Self>) {
        if self.parent.is_none() {
            return;
        }
        if let Some(text) = text_of(&u["content"]) {
            self.child_prompt(text, cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_subagent(
        &mut self,
        id: String,
        name: String,
        task: String,
        prompt: Option<String>,
        host: Option<&str>,
        stand_in: bool,
        cx: &mut Context<Self>,
    ) {
        if self.subagents.iter().any(|s| s.id == id) {
            return;
        }
        if let Some(ix) = self
            .subagents
            .iter()
            .rposition(|s| !s.mirrored && agent_of(&s.id) == agent_of(&id))
        {
            return self.resume_subagent(ix, id, task, prompt, host, stand_in, cx);
        }
        let thread = self.child_thread(&name, cx);
        thread.update(cx, |c, cx| {
            if let Some(p) = prompt {
                c.child_prompt(p, cx);
            }
            c.child_state(SubagentState::Running, cx);
        });
        let host = host
            .filter(|h| self.subagents.iter().any(|s| s.id == *h))
            .map(str::to_string);
        if stand_in {
            let call = marker_call(&id, &name, &task);
            match host
                .as_deref()
                .and_then(|h| self.subagents.iter().find(|s| s.id == h))
            {
                Some(h) => h.thread.clone().update(cx, |c, cx| {
                    c.push_entry(Entry::Tool(call), cx);
                }),
                None => {
                    self.push_entry(Entry::Tool(call), cx);
                }
            }
        }
        self.subagents.push(Subagent {
            id: id.clone(),
            name,
            task,
            state: SubagentState::Running,
            started: unix_now(),
            ended: None,
            mirrored: false,
            call: None,
            earlier: Vec::new(),
            host,
            thread,
        });
        cx.emit(ThreadEvent::SubagentsChanged { spawned: Some(id) });
        self.watch_trails(cx);
        cx.notify();
    }

    /// A new generation `id` of the subagent at `ix`, which SendMessage
    /// resumed: it goes on in the same row and transcript, under its new id.
    #[allow(clippy::too_many_arguments)]
    fn resume_subagent(
        &mut self,
        ix: usize,
        id: String,
        task: String,
        prompt: Option<String>,
        host: Option<&str>,
        stand_in: bool,
        cx: &mut Context<Self>,
    ) {
        let host = host
            .filter(|h| self.subagents.iter().any(|s| s.id == *h))
            .map(str::to_string);
        let s = &mut self.subagents[ix];
        let previous = std::mem::replace(&mut s.id, id.clone());
        s.earlier.push(previous);
        if !task.trim().is_empty() {
            s.task = task.clone();
        }
        s.state = SubagentState::Running;
        s.started = unix_now();
        s.ended = None;
        s.host = host.clone();
        let (name, thread) = (s.name.clone(), s.thread.clone());
        thread.update(cx, |c, cx| {
            match prompt {
                Some(p) => c.child_prompt(p, cx),
                None => c.notice("Resumed", false, cx),
            }
            c.child_state(SubagentState::Running, cx);
        });
        // The parent's transcript marks where it sent the subagent more work.
        if stand_in {
            let call = marker_call(&id, &name, &task);
            match host
                .as_deref()
                .and_then(|h| self.subagents.iter().find(|s| s.id == h))
            {
                Some(h) => h.thread.clone().update(cx, |c, cx| {
                    c.push_entry(Entry::Tool(call), cx);
                }),
                None => {
                    self.push_entry(Entry::Tool(call), cx);
                }
            }
        }
        cx.emit(ThreadEvent::SubagentsChanged { spawned: Some(id) });
        self.watch_trails(cx);
        cx.notify();
    }

    fn set_subagent_state(&mut self, id: &str, state: SubagentState, cx: &mut Context<Self>) {
        let Some(s) = self.subagents.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if s.state == state {
            return;
        }
        s.state = state;
        s.ended = (!state.is_running()).then(unix_now);
        let (thread, host) = (s.thread.clone(), s.host.clone());
        thread.update(cx, |c, cx| c.child_state(state, cx));
        self.set_stand_in(id, host.as_deref(), state, cx);
        if state.is_running() {
            self.watch_trails(cx);
        }
        // Requests a subagent left unanswered go with it.
        if !state.is_running() {
            let reply = self.permission_cancelled();
            let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.permissions)
                .into_iter()
                .partition(|p| p.session.as_deref() == Some(id));
            self.permissions = keep.into();
            for req in gone {
                if let Some(conn) = &self.conn {
                    conn.respond(req.rpc_id.clone(), reply.clone());
                }
                self.after_input(&req.tool_call_id, cx);
            }
            let asked = self.native.questions.len();
            self.drop_questions_where(|_, q| q.session.as_deref() == Some(id), cx);
            if self.native.questions.len() != asked {
                self.after_input("", cx);
            }
        }
        cx.emit(ThreadEvent::SubagentsChanged { spawned: None });
        cx.notify();
    }

    /// Keeps AIR's stand-in Agent call in step with the subagent it stands for.
    fn set_stand_in(
        &mut self,
        id: &str,
        host: Option<&str>,
        state: SubagentState,
        cx: &mut Context<Self>,
    ) {
        let marker = format!("{MARKER}{id}");
        let status = state.call_status();
        let set = move |t: &mut Thread, cx: &mut Context<Thread>| {
            if let Some(&ix) = t.tool_index.get(&marker)
                && let Some(Entry::Tool(call)) = t.entries.get_mut(ix)
                && call.status != status
            {
                call.status = status;
                t.entries_changed(ix, cx);
            }
        };
        match host
            .and_then(|h| self.subagents.iter().find(|s| s.id == h))
            .map(|s| s.thread.clone())
        {
            Some(h) => h.update(cx, set),
            None => set(self, cx),
        }
    }

    // ---- transcript files ------------------------------------------------

    /// Where Claude Code writes the transcript of each running subagent.
    fn trail_paths(&self) -> Vec<(Entity<Thread>, PathBuf)> {
        let Some(session) = self.session_id.as_deref() else {
            return Vec::new();
        };
        self.subagents
            .iter()
            .filter(|s| s.state.is_running() && !s.mirrored)
            .map(|s| {
                let path = subagent_trail::transcript_path(&self.cwd, session, &s.id);
                (s.thread.clone(), path)
            })
            .collect()
    }

    /// Reads the transcripts of Claude's running subagents every few seconds,
    /// until none runs. A session on a server has them there, not here.
    fn watch_trails(&mut self, cx: &mut Context<Self>) {
        if self._trail_watch.is_some()
            || self.remote
            || self.parent.is_some()
            || self.kind != AgentKind::Claude
            || self.trail_paths().is_empty()
        {
            return;
        }
        self._trail_watch = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(paths) = this.update(cx, |t, _| t.trail_paths()) else {
                    return;
                };
                if paths.is_empty() {
                    let _ = this.update(cx, |t, _| t._trail_watch = None);
                    return;
                }
                let (threads, files): (Vec<_>, Vec<_>) = paths.into_iter().unzip();
                let found: Vec<Option<Activity>> = cx
                    .background_executor()
                    .spawn(async move { files.iter().map(|f| subagent_trail::read(f)).collect() })
                    .await;
                let shown = this.update(cx, |_, cx| {
                    for (thread, activity) in threads.into_iter().zip(found) {
                        thread.update(cx, |c, cx| c.set_trail(activity, cx));
                    }
                });
                if shown.is_err() {
                    return;
                }
                cx.background_executor().timer(TRAIL_EVERY).await;
            }
        }));
    }

    /// Takes what a subagent's transcript says, and redraws so its age counts.
    fn set_trail(&mut self, activity: Option<Activity>, cx: &mut Context<Self>) {
        if !self.is_busy() {
            return;
        }
        self.trail = activity;
        cx.notify();
    }

    /// Ends every subagent still running: the agent behind them is gone.
    pub(super) fn stop_subagents(&mut self, cx: &mut Context<Self>) {
        let running: Vec<String> = self
            .subagents
            .iter()
            .filter(|s| s.state.is_running())
            .map(|s| s.id.clone())
            .collect();
        for id in running {
            self.set_subagent_state(&id, SubagentState::Cancelled, cx);
        }
    }

    /// Mirrors a Task call, or a call made under one, into its subagent.
    pub(super) fn mirror_subagent(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.parent.is_some() {
            return;
        }
        let Some(Entry::Tool(t)) = self.entries.get(ix) else {
            return;
        };
        if t.id.starts_with(MARKER) {
            return;
        }
        if t.is_subagent() {
            let (id, state) = (t.id.clone(), SubagentState::of_call(t.status));
            let input = |k: &str| {
                t.raw_input
                    .as_ref()
                    .and_then(|i| str_of(i, k))
                    .filter(|s| !s.trim().is_empty())
            };
            let name = input("description")
                .or_else(|| Some(t.title.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| "Subagent".into());
            let task = input("prompt").unwrap_or_default();
            // A child session that already stands for this call is the subagent.
            if self
                .subagents
                .iter()
                .any(|s| !s.mirrored && s.call.as_deref() == Some(id.as_str()))
            {
                return;
            }
            match self.subagents.iter().position(|s| s.id == id) {
                None => {
                    let thread = self.child_thread(&name, cx);
                    thread.update(cx, |c, cx| {
                        if !task.is_empty() {
                            c.child_prompt(task.clone(), cx);
                        }
                        c.child_state(state, cx);
                    });
                    self.subagents.push(Subagent {
                        id: id.clone(),
                        name,
                        task,
                        state,
                        started: unix_now(),
                        ended: (!state.is_running()).then(unix_now),
                        mirrored: true,
                        call: None,
                        earlier: Vec::new(),
                        host: None,
                        thread,
                    });
                    cx.emit(ThreadEvent::SubagentsChanged { spawned: Some(id) });
                }
                Some(i) => {
                    let s = &mut self.subagents[i];
                    if s.name != name || s.state != state {
                        s.name = name;
                        if s.state != state {
                            s.state = state;
                            s.ended = (!state.is_running()).then(unix_now);
                            let thread = s.thread.clone();
                            thread.update(cx, |c, cx| c.child_state(state, cx));
                        }
                        cx.emit(ThreadEvent::SubagentsChanged { spawned: None });
                    }
                }
            }
            return;
        }
        let Some(parent) = t.parent.as_deref() else {
            return;
        };
        let Some(s) = self.subagents.iter().find(|s| s.mirrored && s.id == parent) else {
            return;
        };
        let (thread, call) = (s.thread.clone(), t.clone());
        thread.update(cx, |c, cx| c.put_tool(call, cx));
    }

    // ---- saving and reopening ----------------------------------------------

    pub fn subagent_records(&self, cx: &gpui::App) -> Vec<SubagentRecord> {
        self.subagents
            .iter()
            .filter(|s| !s.mirrored)
            .map(|s| SubagentRecord {
                id: s.id.clone(),
                name: s.name.clone(),
                task: s.task.clone(),
                state: s.state,
                started: s.started,
                ended: s.ended,
                host: s.host.clone(),
                call: s.call.clone(),
                earlier: s.earlier.clone(),
                entries: s.thread.read(cx).entries().to_vec(),
            })
            .collect()
    }

    /// Takes the subagents a transcript saved, and mirrors the Task calls in
    /// its entries again. `reopened`: this process runs the agent now, so what
    /// was running then is not anymore.
    pub(super) fn load_subagents(
        &mut self,
        records: Vec<SubagentRecord>,
        reopened: bool,
        cx: &mut Context<Self>,
    ) {
        if self.parent.is_some() {
            return;
        }
        let before = self.subagents.len();
        let mut unmirrored = false;
        for r in records {
            // The child session is the subagent its Agent call stands for:
            // drop the mirror made of that call before the record came.
            if let Some(call) = r.call.as_deref()
                && let Some(m) = self
                    .subagents
                    .iter()
                    .position(|s| s.mirrored && s.id == call)
            {
                self.subagents.remove(m);
                unmirrored = true;
            }
            let state = if reopened && r.state.is_running() {
                SubagentState::Cancelled
            } else {
                r.state
            };
            let ended = if state.is_running() {
                None
            } else {
                r.ended.or(Some(r.started))
            };
            match self
                .subagents
                .iter_mut()
                .find(|s| s.id == r.id && !s.mirrored)
            {
                Some(s) => {
                    s.name = r.name;
                    s.task = r.task;
                    s.ended = ended;
                    s.state = state;
                    if r.call.is_some() {
                        s.call = r.call;
                    }
                    s.earlier = r.earlier;
                    let thread = s.thread.clone();
                    // The saved entries already say how it ended.
                    thread.update(cx, |c, cx| {
                        c.show_entries(r.entries, cx);
                        c.follow_state(state, cx);
                    });
                }
                None => {
                    let thread = self.child_thread(&r.name, cx);
                    thread.update(cx, |c, cx| {
                        c.set_entries(r.entries);
                        c.turn_user = c
                            .entries
                            .iter()
                            .position(|e| matches!(e, Entry::User { .. }));
                        c.follow_state(state, cx);
                    });
                    self.subagents.push(Subagent {
                        id: r.id,
                        name: r.name,
                        task: r.task,
                        state,
                        started: r.started,
                        ended,
                        mirrored: false,
                        call: r.call,
                        earlier: r.earlier,
                        host: r.host,
                        thread,
                    });
                }
            }
        }
        // Stand-ins of subagents that ended after the transcript was saved.
        let states: Vec<(String, Option<String>, SubagentState)> = self
            .subagents
            .iter()
            .filter(|s| !s.mirrored)
            .map(|s| (s.id.clone(), s.host.clone(), s.state))
            .collect();
        for (id, host, state) in states {
            self.set_stand_in(&id, host.as_deref(), state, cx);
        }
        self.mirror_all(cx);
        if unmirrored || self.subagents.len() != before {
            cx.emit(ThreadEvent::SubagentsChanged { spawned: None });
        }
        self.watch_trails(cx);
    }

    /// Mirrors every Task call in the transcript, with the time of the prompt it answered.
    fn mirror_all(&mut self, cx: &mut Context<Self>) {
        let mut calls: Vec<(String, u64, Vec<Entry>)> = Vec::new();
        let mut at = unix_now();
        for e in &self.entries {
            match e {
                Entry::User { at: a, .. } => at = *a,
                Entry::Tool(t) if t.is_subagent() && !t.id.starts_with(MARKER) => {
                    calls.push((t.id.clone(), at, Vec::new()))
                }
                Entry::Tool(t) => {
                    if let Some(c) = t
                        .parent
                        .as_deref()
                        .and_then(|p| calls.iter_mut().find(|c| c.0 == p))
                    {
                        c.2.push(e.clone());
                    }
                }
                _ => {}
            }
        }
        for (id, at, mut entries) in calls {
            let Some(&ix) = self.tool_index.get(&id) else {
                continue;
            };
            self.mirror_subagent(ix, cx);
            let Some(s) = self.subagents.iter_mut().find(|s| s.mirrored && s.id == id) else {
                continue;
            };
            s.started = at;
            if !s.state.is_running() {
                s.ended = Some(at);
            }
            if !s.task.is_empty() {
                entries.insert(
                    0,
                    Entry::User {
                        text: s.task.clone(),
                        at,
                        elapsed_ms: None,
                        message_id: None,
                        tokens: None,
                        shell_changes: Vec::new(),
                    },
                );
            }
            let thread = s.thread.clone();
            thread.update(cx, |c, cx| c.show_entries(entries, cx));
        }
    }
}

/// Commands started in the background in `turn` and not stopped in it.
/// Claude Code reports each one's end as a turn of its own, not as an
/// update to the call that started it.
pub(crate) fn background_commands(turn: &[Entry]) -> usize {
    let mut started = 0usize;
    let mut stopped = 0usize;
    for e in turn {
        let Entry::Tool(t) = e else { continue };
        if t.status == ToolStatus::Failed {
            continue;
        }
        let input = t.raw_input.as_ref();
        if input.is_some_and(|i| i["run_in_background"] == Value::Bool(true)) {
            started += 1;
        } else if matches!(
            t.tool_name.as_deref(),
            Some("KillShell" | "KillBash" | "TaskStop")
        ) {
            stopped += 1;
        }
    }
    started.saturating_sub(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, input: Value, status: ToolStatus) -> Entry {
        let mut c = marker_call("x", name, "");
        c.tool_name = Some(name.into());
        c.raw_input = Some(input);
        c.status = status;
        Entry::Tool(c)
    }

    #[test]
    fn a_turn_counts_the_commands_it_left_in_the_background() {
        let bg = || {
            call(
                "Bash",
                json!({"command": "cargo test", "run_in_background": true}),
                ToolStatus::Completed,
            )
        };
        let fg = call("Bash", json!({"command": "ls"}), ToolStatus::Completed);
        assert_eq!(background_commands(std::slice::from_ref(&fg)), 0);
        assert_eq!(background_commands(&[fg, bg(), bg()]), 2);
        // One that failed to start leaves nothing running.
        let failed = call(
            "Bash",
            json!({"run_in_background": true}),
            ToolStatus::Failed,
        );
        assert_eq!(background_commands(&[failed]), 0);
        // One the agent stopped again is not waited for.
        let kill = call("KillShell", json!({"shell_id": "1"}), ToolStatus::Completed);
        assert_eq!(background_commands(&[bg(), kill.clone()]), 0);
        assert_eq!(background_commands(&[kill]), 0);
    }

    #[test]
    fn states_read_from_both_forms() {
        assert_eq!(
            SubagentState::from_air("completed"),
            SubagentState::Completed
        );
        assert_eq!(SubagentState::from_air("failed"), SubagentState::Failed);
        assert_eq!(
            SubagentState::from_air("disconnected"),
            SubagentState::Cancelled
        );
        assert_eq!(
            SubagentState::from_rfd(&json!({"state": "running"})),
            SubagentState::Running
        );
        assert_eq!(
            SubagentState::from_rfd(&json!({"state": "requires_action"})),
            SubagentState::Running
        );
        assert_eq!(
            SubagentState::from_rfd(&json!({"state": "idle", "stopReason": "end_turn"})),
            SubagentState::Completed
        );
        assert_eq!(
            SubagentState::from_rfd(&json!({"state": "idle", "stopReason": "cancelled"})),
            SubagentState::Cancelled
        );
        assert_eq!(
            SubagentState::from_rfd(&json!({"state": "idle", "stopReason": "error"})),
            SubagentState::Failed
        );
    }

    #[test]
    fn agent_calls_name_their_subagent() {
        let stand_in = marker_call("ab12", "Run tests", "cargo test");
        assert_eq!(stand_in.subagent_id(), Some("ab12"));
        assert!(stand_in.is_subagent());
        let mut task = stand_in.clone();
        task.id = "toolu_1".into();
        assert_eq!(task.subagent_id(), Some("toolu_1"));
        let mut bash = task.clone();
        (bash.tool_name, bash.raw_input, bash.title) = (Some("Bash".into()), None, "ls".into());
        assert_eq!(bash.subagent_id(), None);
    }

    #[test]
    fn a_resumed_generation_runs_the_same_agent() {
        assert_eq!(agent_of("a0d2:generation:2"), "a0d2");
        assert_eq!(agent_of("a0d2:generation:1760000000000"), "a0d2");
        assert_eq!(agent_of("a0d2"), "a0d2");
        assert_ne!(agent_of("a0d2"), agent_of("b1c3:generation:2"));
    }

    #[test]
    fn a_record_keeps_its_earlier_generations() {
        let old: SubagentRecord = serde_json::from_value(json!({
            "id": "a1", "name": "Fix", "state": "completed", "started": 5
        }))
        .unwrap();
        assert!(old.earlier.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("earlier"));
        let resumed = SubagentRecord {
            id: "a1:generation:2".into(),
            earlier: vec!["a1".into()],
            ..old
        };
        let back: SubagentRecord =
            serde_json::from_str(&serde_json::to_string(&resumed).unwrap()).unwrap();
        assert_eq!(back.earlier, ["a1"]);
    }

    #[test]
    fn prompts_come_as_text_or_blocks() {
        assert_eq!(text_of(&json!("do it")).as_deref(), Some("do it"));
        assert_eq!(
            text_of(&json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]))
                .as_deref(),
            Some("a\nb")
        );
        assert_eq!(text_of(&json!("  ")), None);
    }
}
