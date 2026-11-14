//! Session actions over ACP, for Claude Code: compact, fork and rename, the
//! way native Droid has them (see `native.rs`). Claude's adapter takes
//! `/compact` as a prompt and reports it with `compaction_update`, forks with
//! `session/fork` and renames with the AIR `_session/rename` extension. A
//! rewind puts back the files from the transcript's own diffs, then forks the
//! session at the message before the prompt.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context};
use serde_json::{Value, json};

use super::native::Deferred;
use super::{
    Backend, Entry, RewindPlan, SessionMeta, Thread, ThreadEvent, ThreadStatus, ToolContent,
    ToolStatus, str_of, unix_now,
};

const COMPACTING: &str = "Compacting the conversation…";

impl Thread {
    /// ACP, connected and with a session.
    pub(super) fn acp_live(&self) -> bool {
        self.backend == Backend::Acp
            && self.conn.is_some()
            && self.session_id.is_some()
            && !matches!(self.status, ThreadStatus::Connecting)
    }

    /// Whether the agent can copy its sessions (`session/fork`); native Droid always can.
    pub fn supports_fork(&self) -> bool {
        self.backend == Backend::Native || self.caps.fork
    }

    /// Runs what was asked for before the session existed.
    pub(super) fn acp_session_ready(&mut self, cx: &mut Context<Self>) {
        if let Some(title) = self.native.want_title.take() {
            self.acp_send_title(title, cx);
        }
        for action in std::mem::take(&mut self.native.deferred) {
            match action {
                Deferred::Fork => self.fork(cx),
                Deferred::Compact => self.compact(None, cx),
            }
        }
    }

    /// `/compact` as a prompt of its own, without a bubble in the transcript;
    /// `compaction_update` marks where it happened.
    pub(super) fn acp_compact(&mut self, instructions: Option<String>, cx: &mut Context<Self>) {
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        if self.is_busy() {
            return;
        }
        let text = match instructions {
            Some(i) if !i.trim().is_empty() => format!("/compact {}", i.trim()),
            _ => "/compact".to_string(),
        };
        self.set_status(ThreadStatus::Working, cx);
        self.status_detail = Some(COMPACTING.into());
        self.native.compacting = true;
        self.native.compact_before = self.usage.context_percent();
        self.turn_usage = self.usage.clone();
        let start = self.entries.len();
        let request = conn.request(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
        );
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| {
                t.status_detail = None;
                t.native.compacting = false;
                match result {
                    Ok(v) => {
                        t.add_turn_tokens(&v);
                        let marked = t
                            .entries
                            .get(start..)
                            .unwrap_or(&[])
                            .iter()
                            .any(|e| matches!(e, Entry::Compacted { .. }));
                        if !marked && v["stopReason"].as_str() != Some("cancelled") {
                            let before = t.native.compact_before.take();
                            t.push_entry(
                                Entry::Compacted {
                                    removed: 0,
                                    before,
                                    after: None,
                                },
                                cx,
                            );
                        }
                    }
                    Err(e) => t.notice(format!("Compacting failed: {e}"), true, cx),
                }
                t.native.compact_before = None;
                t.log_turn_usage(None, cx);
                t.set_status(ThreadStatus::Idle, cx);
                t.meta_changed(cx);
                t.drain_queue(cx);
            });
        })
        .detach();
    }

    /// A `compaction_update`: the agent summarizing the conversation, asked for
    /// or on its own when the context fills up.
    pub(super) fn acp_compaction_update(&mut self, u: &Value, cx: &mut Context<Self>) {
        if self.replaying && self.suppress_replay {
            return;
        }
        let facts = &u["_meta"]["jetbrains"]["air"]["contextCompaction"];
        match u["status"].as_str().unwrap_or("") {
            "in_progress" => {
                if self.native.compact_before.is_none() && !self.replaying {
                    self.native.compact_before = self.usage.context_percent();
                }
                if !self.replaying {
                    self.status_detail = Some(COMPACTING.into());
                    self.meta_changed(cx);
                }
            }
            "completed" => {
                let size = self.usage.context_size.filter(|&s| s > 0);
                let pct = |n: u64, s: u64| ((n * 100 + s / 2) / s).min(100);
                let before = self
                    .native
                    .compact_before
                    .take()
                    .or_else(|| Some(pct(facts["preTokens"].as_u64()?, size?)))
                    .filter(|_| !self.replaying);
                let post = facts["postTokens"].as_u64();
                let after = match (post, size) {
                    (Some(p), Some(s)) if !self.replaying => {
                        self.usage.context_used = Some(p);
                        Some(pct(p, s))
                    }
                    _ => None,
                };
                if !self.native.compacting {
                    self.status_detail = None;
                }
                self.push_entry(
                    Entry::Compacted {
                        removed: 0,
                        before,
                        after,
                    },
                    cx,
                );
                self.meta_changed(cx);
            }
            "failed" => {
                self.native.compact_before = None;
                if !self.replaying {
                    let why =
                        str_of(u, "error").unwrap_or_else(|| "the agent gave no reason".into());
                    self.notice(format!("Compacting failed: {why}"), true, cx);
                }
            }
            "cancelled" => {
                self.native.compact_before = None;
                if !self.native.compacting {
                    self.status_detail = None;
                    self.meta_changed(cx);
                }
            }
            _ => {}
        }
    }

    /// The context left after the last compaction, once the agent reports it.
    pub(super) fn fill_compacted_after(&mut self, cx: &mut Context<Self>) {
        let pct = self.usage.context_percent();
        let recent = self.entries.len().saturating_sub(3);
        if let Some(ix) = (recent..self.entries.len())
            .rev()
            .find(|&i| matches!(self.entries[i], Entry::Compacted { after: None, .. }))
            && let Entry::Compacted { after, .. } = &mut self.entries[ix]
        {
            *after = pct;
            self.entries_changed(ix, cx);
        }
    }

    /// `session/fork`: a copy of the session under a new id; emits [`ThreadEvent::Forked`].
    pub(super) fn acp_fork(&mut self, cx: &mut Context<Self>) {
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        if !self.caps.fork {
            return self.notice(
                format!("{} cannot fork sessions.", self.kind.label()),
                true,
                cx,
            );
        }
        let title = format!("{} (fork)", self.title());
        let cwd = self.cwd.clone();
        let request = conn.request(
            "session/fork",
            json!({"sessionId": session_id, "cwd": cwd, "mcpServers": []}),
        );
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| match result {
                Ok(v) => {
                    let Some(id) = str_of(&v, "sessionId") else {
                        return t.notice(
                            "Forking failed: the agent returned no session.",
                            true,
                            cx,
                        );
                    };
                    let now = unix_now();
                    cx.emit(ThreadEvent::Forked(SessionMeta {
                        agent: t.kind,
                        session_id: Some(id),
                        title,
                        cwd: t.cwd.clone(),
                        created: now,
                        updated: now,
                        pr: None,
                    }));
                }
                Err(e) => t.notice(format!("Forking failed: {e}"), true, cx),
            });
        })
        .detach();
    }

    /// Names the session in Claude Code's own list too (`_session/rename`).
    pub(super) fn acp_send_title(&mut self, title: String, cx: &mut Context<Self>) {
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        if !self.caps.rename {
            return;
        }
        let request = conn.request(
            "_session/rename",
            json!({"sessionId": session_id, "title": title}),
        );
        cx.spawn(async move |this, cx| {
            if let Err(e) = request.await {
                let _ = this.update(cx, |t, cx| {
                    let agent = t.kind.label();
                    t.notice(format!("Renaming in {agent} failed: {e}"), true, cx)
                });
            }
        })
        .detach();
    }
    /// What a prompt sent now is rewound to: the agent's last message, or ""
    /// before anything was said. None when that is not known (an agent that
    /// cannot fork, a reopened history without message ids).
    pub(super) fn acp_rewind_point(&self) -> Option<String> {
        if self.backend != Backend::Acp {
            return None;
        }
        if let Some(id) = &self.fork_point {
            return Some(id.clone());
        }
        let said = self
            .entries
            .iter()
            .any(|e| matches!(e, Entry::User { .. } | Entry::Agent { .. } | Entry::Tool(_)));
        (!said).then(String::new)
    }

    /// Lists what going back to before the prompt at `ix` undoes: the files
    /// the agent's edits and shell commands changed since, as they were then.
    pub(super) fn acp_begin_rewind(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Entry::User {
            text,
            message_id: Some(point),
            ..
        }) = self.entries.get(ix)
        else {
            return;
        };
        let (restore, delete) = rewind_files(&self.entries[ix..], &self.cwd);
        self.native.rewind = Some(RewindPlan {
            entry: ix,
            message_id: point.clone(),
            text: text.clone(),
            restore,
            delete,
            evicted: Vec::new(),
            loading: false,
            running: false,
        });
        cx.emit(ThreadEvent::RewindChanged { entry: ix });
        cx.notify();
    }

    /// Puts the files back, then forks the session at the message before the
    /// prompt (a new session for the first one); emits [`ThreadEvent::Rewound`].
    pub(super) fn acp_confirm_rewind(&mut self, cx: &mut Context<Self>) {
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        let Some(plan) = self
            .native
            .rewind
            .as_mut()
            .filter(|p| !p.loading && !p.running)
        else {
            return;
        };
        plan.running = true;
        let (entry, point) = (plan.entry, plan.message_id.clone());
        let (restore, delete) = (plan.restore.clone(), plan.delete.clone());
        let title = format!("{} (rewind)", self.meta.title);
        let cwd = self.cwd.clone();
        cx.emit(ThreadEvent::RewindChanged { entry });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let failed = cx
                .background_spawn(async move { put_back(&restore, &delete) })
                .await;
            let new_id = if point.is_empty() {
                Ok(None)
            } else {
                let params = json!({
                    "sessionId": session_id,
                    "cwd": cwd,
                    "mcpServers": [],
                    "_meta": {"jetbrains": {"air": {"fork": {"version": 1, "messageId": point}}}},
                });
                conn.request("session/fork", params)
                    .await
                    .map(|v| str_of(&v, "sessionId"))
            };
            let _ = this.update(cx, |t, cx| {
                let plan = t.native.rewind.take();
                cx.emit(ThreadEvent::RewindChanged { entry });
                cx.notify();
                if failed > 0 {
                    t.notice(
                        format!("Rewind could not put back {failed} file(s); check them by hand."),
                        true,
                        cx,
                    );
                }
                let session_id = match new_id {
                    Ok(id) => id,
                    Err(e) => {
                        return t.notice(
                            format!("Rewind put the files back, but forking failed: {e}"),
                            true,
                            cx,
                        );
                    }
                };
                let now = unix_now();
                cx.emit(ThreadEvent::Rewound {
                    meta: SessionMeta {
                        agent: t.kind,
                        session_id,
                        title,
                        cwd: t.cwd.clone(),
                        created: now,
                        updated: now,
                        pr: None,
                    },
                    prompt: plan.map(|p| p.text).unwrap_or_default(),
                });
            });
        })
        .detach();
    }
}

/// The files `entries` (a prompt and what came after it) changed, as they
/// were before: `{filePath, content}` to write back, and `{filePath}` for
/// files that did not exist yet. The first change of a file tells how it was.
fn rewind_files(entries: &[Entry], cwd: &Path) -> (Vec<Value>, Vec<Value>) {
    let mut seen = HashSet::new();
    let (mut restore, mut delete) = (Vec::new(), Vec::new());
    let mut note = |path: PathBuf, old: Option<&String>| {
        let path = if path.is_relative() {
            cwd.join(path)
        } else {
            path
        };
        if !seen.insert(path.clone()) {
            return;
        }
        let file = path.to_string_lossy().into_owned();
        match old {
            Some(content) => restore.push(json!({"filePath": file, "content": content})),
            None => delete.push(json!({"filePath": file})),
        }
    };
    for e in entries {
        match e {
            Entry::User { shell_changes, .. } => {
                for c in shell_changes {
                    note(c.path.clone(), c.old.as_ref());
                }
            }
            Entry::Tool(t) if t.status == ToolStatus::Completed => {
                for c in &t.content {
                    if let ToolContent::Diff { path, old_text, .. } = c {
                        note(path.clone(), old_text.as_ref());
                    }
                }
            }
            _ => {}
        }
    }
    (restore, delete)
}

/// Writes back and deletes what [`rewind_files`] listed; how many failed.
fn put_back(restore: &[Value], delete: &[Value]) -> usize {
    let mut failed = 0;
    for f in restore {
        let (Some(path), Some(content)) = (f["filePath"].as_str(), f["content"].as_str()) else {
            continue;
        };
        let path = Path::new(path);
        let ok = path
            .parent()
            .is_none_or(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(path, content).is_ok();
        failed += usize::from(!ok);
    }
    for f in delete {
        let Some(path) = f["filePath"].as_str() else {
            continue;
        };
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            failed += 1;
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewind_takes_each_file_as_it_first_was() {
        let user = |changes: Vec<super::super::ShellChange>| Entry::User {
            text: "p".into(),
            at: 0,
            elapsed_ms: None,
            message_id: Some(String::new()),
            tokens: None,
            shell_changes: changes,
        };
        let edit =
            |path: &str, old: Option<&str>, status: ToolStatus| {
                Entry::Tool(serde_json::from_value(json!({
                "id": path, "title": "Edit", "kind": "edit", "status": status,
                "content": [{"type": "diff", "path": path, "old_text": old, "new_text": "new"}],
            })).unwrap())
            };
        let entries = vec![
            user(Vec::new()),
            edit("a.rs", Some("one"), ToolStatus::Completed),
            edit("a.rs", Some("two"), ToolStatus::Completed),
            edit("b.rs", None, ToolStatus::Completed),
            edit("c.rs", Some("x"), ToolStatus::Failed),
        ];
        let (restore, delete) = rewind_files(&entries, Path::new("/p"));
        assert_eq!(
            restore,
            vec![json!({"filePath": "/p/a.rs", "content": "one"})]
        );
        assert_eq!(delete, vec![json!({"filePath": "/p/b.rs"})]);
    }
}
