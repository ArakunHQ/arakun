//! The chat's side of `request_secret` (`super::secret`): the requests its
//! agent waits on, and how each ends. Values pass through here only on their
//! way to [`secret::keep`]; no entry, event or saved file holds one. A
//! server's session shows the request its server sends, and the answer goes
//! back there ([`RemoteAction::Secret`]), only over a private connection.

use std::collections::VecDeque;
use std::path::Path;

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::{Context, Task};

use super::super::secret::{self, McpCommand, Outcome, Route, SecretRequest, Typed, Why};
use super::{AgentKind, RemoteAction, Thread, ThreadEvent, ThreadStatus};

/// A request on show, and the agent's tool call waiting on it.
pub(super) struct Pending {
    request: SecretRequest,
    reply: oneshot::Sender<Outcome>,
    _timeout: Option<Task<()>>,
}

/// The request a server's session shows, as its server sent it.
struct RemoteSecret {
    request: SecretRequest,
    /// Whether every connection between here and the agent is encrypted.
    private: bool,
    /// Answered here, and the answer is on its way: the card is gone.
    answered: bool,
}

/// What a chat keeps for its requests.
#[derive(Default)]
pub(super) struct Secrets {
    /// A server's session: the request its server shows.
    remote: Option<RemoteSecret>,
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

    /// A device's answer: declines with `None`, or saves the values. Values
    /// that came over a path that is not `private` are refused, and the
    /// request stays open. True when a request ended.
    fn answer(
        &mut self,
        id: u64,
        values: Option<Typed>,
        private: bool,
        cwd: &Path,
    ) -> Result<bool, String> {
        match secret::answer(values, private)? {
            Some(values) => self.save(id, values.0, cwd).map(|()| true),
            None => Ok(self.end(id, Outcome::Declined)),
        }
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
        self.secrets
            .pending
            .front()
            .map(|p| &p.request)
            .or_else(|| {
                let r = self.secrets.remote.as_ref().filter(|r| !r.answered)?;
                Some(&r.request)
            })
    }

    /// Whether what is typed into the card may go: always for a session
    /// here, and for a server's session when its connection is encrypted.
    pub fn secret_private(&self) -> bool {
        self.secrets.remote.as_ref().is_none_or(|r| r.private)
    }

    /// What a server's session waits on, as the server sent it
    /// (`phone::wire::secret`). `connection_private` says whether this app
    /// reaches the server encrypted.
    pub fn set_remote_secret(
        &mut self,
        wire: &serde_json::Value,
        connection_private: bool,
        cx: &mut Context<Self>,
    ) {
        let old = self.secrets.remote.take();
        self.secrets.remote = SecretRequest::from_wire(wire).map(|(request, private)| {
            let answered = old
                .as_ref()
                .is_some_and(|o| o.answered && o.request.id == request.id);
            RemoteSecret {
                request,
                private: private && connection_private,
                answered,
            }
        });
        let asked = self
            .secrets
            .remote
            .as_ref()
            .is_some_and(|r| old.as_ref().is_none_or(|o| o.request.id != r.request.id));
        if asked {
            cx.emit(ThreadEvent::NeedsPermission);
        }
        cx.emit(ThreadEvent::Changed);
        cx.notify();
    }

    /// The answer to a server's session did not reach the server: the card
    /// shows again.
    pub(super) fn remote_secret_undelivered(&mut self) {
        if let Some(r) = &mut self.secrets.remote {
            r.answered = false;
        }
    }

    /// Answers the card: saves `values`, one per field, or declines with
    /// `None`. `private` says whether they came over an encrypted path (the
    /// card here always does). On an error the card stays, so nothing is lost.
    pub fn answer_secret(
        &mut self,
        id: u64,
        values: Option<Typed>,
        private: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let private = private && self.secret_private();
        if self.remote {
            let values = secret::answer(values, private)?;
            let r = self
                .secrets
                .remote
                .as_mut()
                .filter(|r| r.request.id == id && !r.answered)
                .ok_or("The agent stopped waiting for this.")?;
            if let Some(v) = &values
                && (v.0.len() != r.request.fields.len() || v.0.iter().any(String::is_empty))
            {
                return Err("Fill in every field.".into());
            }
            r.answered = true;
            cx.emit(ThreadEvent::Remote(RemoteAction::Secret { id, values }));
            cx.emit(ThreadEvent::Changed);
            cx.notify();
            return Ok(());
        }
        if self.secrets.answer(id, values, private, &self.cwd)? {
            self.secret_ended(cx);
        }
        Ok(())
    }

    /// Saves what the user typed, one value per field, and hands the agent
    /// a handle for each. On an error the card stays, so nothing is lost.
    pub fn save_secret(
        &mut self,
        id: u64,
        values: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.answer_secret(id, Some(Typed(values)), true, cx)
    }

    pub fn decline_secret(&mut self, id: u64, cx: &mut Context<Self>) {
        let _ = self.answer_secret(id, None, true, cx);
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
    fn values_from_an_unencrypted_path_are_refused() {
        const VALUE: &str = "token-sent-in-the-clear";
        let mut secrets = Secrets::default();
        let (_route, mut answer, asked) = ask(&json!({"label": "Token", "reason": "To deploy."}));
        let id = asked.request.id;
        secrets.push(asked, None);

        let refused = secrets.answer(id, Some(Typed(vec![VALUE.into()])), false, Path::new("/p"));
        assert_eq!(refused, Err(secret::NOT_PRIVATE.to_string()));
        assert_eq!(secrets.pending.len(), 1, "the card stays open");
        assert!(
            matches!(answer.try_recv(), Ok(None)),
            "the agent still waits"
        );

        // Over the relay or Tailscale it goes.
        assert_eq!(
            secrets.answer(id, Some(Typed(vec![VALUE.into()])), true, Path::new("/p")),
            Ok(true)
        );
        let Some(Outcome::Saved(handles)) = answer.try_recv().unwrap() else {
            panic!("not saved");
        };
        assert_eq!(
            secret::take(&handles[0].1, Path::new("/p")).as_deref(),
            Some(VALUE)
        );

        // Declining sends nothing, so a plain path may carry it.
        let (_b, mut declined, asked) = ask(&json!({"label": "PIN", "reason": "To pair."}));
        let id = asked.request.id;
        secrets.push(asked, None);
        assert_eq!(secrets.answer(id, None, false, Path::new("/p")), Ok(true));
        assert_eq!(declined.try_recv().unwrap(), Some(Outcome::Declined));
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
