//! The chat's side of `request_secret` (`super::secret`): the requests its
//! agent waits on, and how each ends. Values pass through here only on their
//! way to [`secret::keep`]; no entry, event or saved file holds one.

use std::collections::VecDeque;
use std::path::Path;

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::{Context, Task};

use super::super::secret::{self, McpCommand, Outcome, Route, SecretRequest, Why};
use super::{AgentKind, Thread, ThreadEvent, ThreadStatus};

/// A request on show, and the agent's tool call waiting on it.
pub(super) struct Pending {
    request: SecretRequest,
    reply: oneshot::Sender<Outcome>,
    _timeout: Option<Task<()>>,
}

/// What a chat keeps for its requests.
#[derive(Default)]
pub(super) struct Secrets {
    /// Where this chat's calls arrive while its agent runs.
    route: Option<Route>,
    /// Arakun's MCP server for this chat's session, when there is one.
    pub(super) server: Option<McpCommand>,
    _pump: Option<Task<()>>,
    pending: VecDeque<Pending>,
}

impl Secrets {
    fn push(&mut self, asked: secret::Asked, timeout: Option<Task<()>>) {
        self.pending.push_back(Pending {
            request: asked.request,
            reply: asked.reply,
            _timeout: timeout,
        });
    }

    /// Keeps what the user typed for the project in `cwd`, one value per
    /// field, and answers the agent with a handle for each. On an error the
    /// request stays open.
    fn save(&mut self, id: u64, values: Vec<String>, cwd: &Path) -> Result<(), String> {
        let at = self
            .pending
            .iter()
            .position(|p| p.request.id == id)
            .ok_or("The agent stopped waiting for this.")?;
        let request = &self.pending[at].request;
        if values.len() != request.fields.len() || values.iter().any(|v| v.is_empty()) {
            return Err("Fill in every field.".into());
        }
        let named = request
            .fields
            .iter()
            .map(|f| f.name.clone())
            .zip(values)
            .collect();
        let handles = secret::keep(cwd, named)?;
        self.end(id, Outcome::Saved(handles));
        Ok(())
    }

    /// Answers one request; false when it was not open.
    fn end(&mut self, id: u64, outcome: Outcome) -> bool {
        let Some(at) = self.pending.iter().position(|p| p.request.id == id) else {
            return false;
        };
        if let Some(p) = self.pending.remove(at) {
            let _ = p.reply.send(outcome);
        }
        true
    }

    /// Answers every request as cancelled; false when none was open.
    fn cancel(&mut self, why: Why) -> bool {
        let open = !self.pending.is_empty();
        for p in std::mem::take(&mut self.pending) {
            let _ = p.reply.send(Outcome::Cancelled(why));
        }
        open
    }
}

impl Thread {
    /// A fresh address for this chat's requests, for the agent about to start.
    /// Requests to the old one end as cancelled. Returns the token for the
    /// server's environment.
    pub(super) fn open_secret_route(&mut self, cx: &mut Context<Self>) -> Option<String> {
        self.close_secret_route(cx);
        if self.remote || !secret::offered_to(self.kind) {
            return None;
        }
        let (route, mut rx) = Route::open()?;
        let token = route.token().to_string();
        self.secrets.route = Some(route);
        self.secrets._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(asked) = rx.next().await {
                if this.update(cx, |t, cx| t.secret_asked(asked, cx)).is_err() {
                    break;
                }
            }
        }));
        Some(token)
    }

    /// Ends every request and closes the address.
    pub(super) fn close_secret_route(&mut self, cx: &mut Context<Self>) {
        self.cancel_secrets(Why::Closed, cx);
        self.secrets.route = None;
        self.secrets.server = None;
        self.secrets._pump = None;
    }

    fn secret_asked(&mut self, asked: secret::Asked, cx: &mut Context<Self>) {
        let id = asked.request.id;
        let timer = cx.background_executor().timer(asked.request.timeout);
        let timeout = cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |t, cx| {
                t.end_secret(id, Outcome::Cancelled(Why::TimedOut), cx)
            });
        });
        self.secrets.push(asked, Some(timeout));
        if self.status == ThreadStatus::Working {
            self.set_status(ThreadStatus::NeedsInput, cx);
        }
        cx.emit(ThreadEvent::NeedsPermission);
        cx.notify();
    }

    /// The request the card shows: the oldest one still open.
    pub fn pending_secret(&self) -> Option<&SecretRequest> {
        self.secrets.pending.front().map(|p| &p.request)
    }

    /// Saves what the user typed, one value per field, and hands the agent
    /// a handle for each. On an error the card stays, so nothing is lost.
    pub fn save_secret(
        &mut self,
        id: u64,
        values: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.secrets.save(id, values, &self.cwd)?;
        self.secret_ended(cx);
        Ok(())
    }

    pub fn decline_secret(&mut self, id: u64, cx: &mut Context<Self>) {
        self.end_secret(id, Outcome::Declined, cx);
    }

    /// Ends every open request without an answer.
    pub(super) fn cancel_secrets(&mut self, why: Why, cx: &mut Context<Self>) {
        if !self.secrets.cancel(why) {
            return;
        }
        // Callers settle the turn; this only stops saying it waits on the user.
        if self.status == ThreadStatus::NeedsInput {
            let status = self.resting_status();
            self.set_status(status, cx);
        }
        cx.emit(ThreadEvent::Changed);
        cx.notify();
    }

    fn end_secret(&mut self, id: u64, outcome: Outcome, cx: &mut Context<Self>) {
        if self.secrets.end(id, outcome) {
            self.secret_ended(cx);
        }
    }

    fn secret_ended(&mut self, cx: &mut Context<Self>) {
        self.after_input("", cx);
        cx.emit(ThreadEvent::Changed);
        cx.notify();
    }

    /// Arakun's MCP server, as this session's `mcpServers` over ACP. Codex has
    /// it in its config instead, with a longer tool timeout.
    pub(super) fn acp_mcp_servers(&self) -> serde_json::Value {
        match &self.secrets.server {
            Some(server) if self.kind != AgentKind::Codex => serde_json::json!([server.acp()]),
            _ => serde_json::json!([]),
        }
    }

    /// The same for Droid's own protocol.
    pub(super) fn droid_mcp_servers(&self) -> serde_json::Value {
        match &self.secrets.server {
            Some(server) => serde_json::json!([server.droid()]),
            None => serde_json::json!([]),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{
        Entry, SessionMeta, ToolCall, ToolStatus, Transcript, Usage, parse_tool_content,
    };
    use super::*;

    fn ask(args: &serde_json::Value) -> (Route, oneshot::Receiver<Outcome>, secret::Asked) {
        let (route, mut rx) = Route::open().unwrap();
        let answer = secret::ask(route.token(), args).unwrap();
        let asked = rx.try_recv().unwrap();
        (route, answer, asked)
    }

    fn thread_file(meta: SessionMeta, entries: Vec<Entry>, queue: Vec<String>) -> String {
        serde_json::to_string(&Transcript {
            version: 1,
            meta,
            entries,
            usage: Usage::default(),
            fork_point: None,
            interrupted: false,
            resume: false,
            queue,
            handoff: None,
            subagents: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn no_field_value_reaches_the_transcript_or_the_thread_file() {
        const USER: &str = "octocat-not-in-the-session";
        const PASSWORD: &str = "hunter2-not-in-the-session";
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        let args = json!({
            "label": "GitHub login",
            "reason": "To push the branch.",
            "fields": [{"name": "user", "label": "User name"}, {"name": "password"}],
        });
        let (_route, mut answer, asked) = ask(&args);
        let id = asked.request.id;
        t.secrets.push(asked, None);
        assert_eq!(t.pending_secret().unwrap().label, "GitHub login");

        t.secrets
            .save(id, vec![USER.into(), PASSWORD.into()], &t.cwd)
            .unwrap();
        assert!(t.pending_secret().is_none());

        // What Arakun's MCP server answers, and the agent shows as the call's result.
        let outcome = answer.try_recv().unwrap().unwrap();
        let call = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": secret::TOOL, "arguments": args}})
        .to_string();
        let reply = secret::mcp_reply(&call, |_| Ok(outcome.to_json())).unwrap();
        let result = &reply["result"];
        let content: Vec<_> = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| json!({"type": "content", "content": c}))
            .collect();
        t.entries.push(Entry::Tool(ToolCall {
            id: "toolu_1".into(),
            title: "mcp__arakun__request_secret".into(),
            kind: "other".into(),
            status: ToolStatus::Completed,
            content: parse_tool_content(&json!(content)),
            locations: Vec::new(),
            raw_input: Some(args.clone()),
            raw_output: Some(result.to_string()),
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
        }));

        let saved = thread_file(t.meta(), t.entries.clone(), t.queue.clone());
        for value in [USER, PASSWORD] {
            assert!(!saved.contains(value), "{value} is in the thread file");
            assert!(
                !reply.to_string().contains(value),
                "{value} went to the agent"
            );
        }
        assert!(saved.contains("secret-ref:"), "the outcome is recorded");

        // The values wait behind the handles, for this project only, once.
        let Outcome::Saved(handles) = outcome else {
            panic!("not saved: {outcome:?}");
        };
        assert_eq!(handles[0].0, "user");
        let password = &handles[1].1;
        assert_eq!(secret::take(password, Path::new("/elsewhere")), None);
        assert_eq!(
            secret::take(password, Path::new("/p")).as_deref(),
            Some(PASSWORD)
        );
        assert_eq!(secret::take(password, Path::new("/p")), None);
    }

    #[test]
    fn a_failed_save_keeps_the_request_open() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        let (_route, mut answer, asked) = ask(&json!({"label": "Token", "reason": "To deploy."}));
        let id = asked.request.id;
        t.secrets.push(asked, None);
        assert!(t.secrets.save(id, vec![String::new()], &t.cwd).is_err());
        assert!(t.secrets.save(id + 1000, vec!["x".into()], &t.cwd).is_err());
        assert!(t.pending_secret().is_some());
        assert!(
            matches!(answer.try_recv(), Ok(None)),
            "the agent still waits"
        );
    }

    #[test]
    fn declining_and_cancelling_tell_the_agent() {
        let mut secrets = Secrets::default();
        let (_a, mut declined, asked) = ask(&json!({"label": "PIN", "reason": "To pair."}));
        let id = asked.request.id;
        secrets.push(asked, None);
        assert!(secrets.end(id, Outcome::Declined));
        assert_eq!(declined.try_recv().unwrap(), Some(Outcome::Declined));

        let (_b, mut first, asked1) = ask(&json!({"label": "A", "reason": "r"}));
        let (_c, mut second, asked2) = ask(&json!({"label": "B", "reason": "r"}));
        secrets.push(asked1, None);
        secrets.push(asked2, None);
        assert!(secrets.cancel(Why::NewMessage));
        assert!(!secrets.cancel(Why::NewMessage), "nothing left open");
        for answer in [&mut first, &mut second] {
            assert_eq!(
                answer.try_recv().unwrap(),
                Some(Outcome::Cancelled(Why::NewMessage))
            );
        }
    }
}
