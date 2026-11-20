//! Terminals in a server's environments: the shell runs on the server
//! (`server/pty.rs`) and its bytes come over the paired connection; the
//! dock's [`Terminal`] draws them as it does a local one's.
//!
//! The app names each terminal. After a drop it follows them again from
//! the last byte it drew, so a dev server left running there is still
//! there, with what it printed meanwhile.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use std::sync::Arc;

use gpui::{App, WeakEntity};

use super::*;
use crate::terminal::{RemoteOut, RemoteSink, Terminal};

/// The messages about a terminal after its `term_open`.
const TERM_KINDS: [&str; 5] = [
    "term_attach",
    "term_input",
    "term_resize",
    "term_seen",
    "term_close",
];

/// What a terminal sends the server, by server and terminal id.
type Out = (String, String, RemoteOut);

/// The terminals open on servers.
pub(super) struct Terms {
    open: HashMap<String, RemoteTerm>,
    out: mpsc::UnboundedSender<Out>,
    /// Terminals closed while their server was away: it hears once it is back.
    closed_away: Vec<(String, String)>,
    _out: Task<()>,
}

struct RemoteTerm {
    server: String,
    terminal: WeakEntity<Terminal>,
}

impl Terms {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Terms {
        let (out, mut rx) = mpsc::unbounded::<Out>();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(next) = rx.try_recv() {
                    batch.push(next);
                }
                if this
                    .update(cx, |this, _| this.server_terms_send(batch))
                    .is_err()
                {
                    break;
                }
            }
        });
        Terms {
            open: HashMap::new(),
            out,
            closed_away: Vec::new(),
            _out: task,
        }
    }
}

/// Keys typed in a row go out as one message.
fn coalesce(batch: Vec<Out>) -> Vec<Out> {
    let mut merged: Vec<Out> = Vec::with_capacity(batch.len());
    for (server, term, out) in batch {
        if let (Some((s, t, RemoteOut::Input(before))), RemoteOut::Input(more)) =
            (merged.last_mut(), &out)
            && *s == server
            && *t == term
        {
            before.extend_from_slice(more);
            continue;
        }
        merged.push((server, term, out));
    }
    merged
}

fn message(term: &str, out: &RemoteOut) -> Value {
    match out {
        RemoteOut::Input(bytes) => {
            json!({ "type": "term_input", "term": term, "data": B64.encode(bytes) })
        }
        RemoteOut::Resize { cols, rows } => {
            json!({ "type": "term_resize", "term": term, "cols": cols, "rows": rows })
        }
        RemoteOut::Close => json!({ "type": "term_close", "term": term }),
    }
}

impl Shell {
    /// A terminal in the server's environment `env_path`, running `run` in
    /// its shell when given. Returns its index in that dock.
    pub(in crate::shell) fn spawn_server_terminal(
        &mut self,
        env_path: &Path,
        run: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let remote = self.find_env(env_path)?.remote.clone()?;
        let title = "Could not open a terminal";
        if self.connected(&remote.server).is_none() {
            let body = format!("{} is not connected. Try again once it is.", remote.machine);
            self.notify(crate::island::AlertKind::Warning, title, &body, None, cx);
            return None;
        }
        let link = self.servers.as_mut()?;
        let term = servers::new_id();
        let sink: RemoteSink = {
            let (out, server, term) = (link.terms.out.clone(), remote.server.clone(), term.clone());
            Arc::new(move |msg| {
                let _ = out.unbounded_send((server.clone(), term.clone(), msg));
            })
        };
        let terminal = Terminal::remote(PathBuf::from(&remote.place), sink, cx);
        link.terms.open.insert(
            term.clone(),
            RemoteTerm {
                server: remote.server.clone(),
                terminal: terminal.downgrade(),
            },
        );
        let (cols, rows) = terminal.read(cx).grid_size();
        if let Some(conn) = link.conns.get(&remote.server) {
            conn.client.send(&json!({
                "type": "term_open", "term": term, "place": remote.place,
                "run": run, "cols": cols, "rows": rows,
            }));
        }
        self.add_terminal(env_path, terminal, focus, window, cx)
    }

    /// Whether `server` answers now, so a terminal can open there.
    pub(in crate::shell) fn server_connected(&self, server: &str) -> bool {
        self.connected(server).is_some()
    }

    /// Sends what the terminals had to say.
    fn server_terms_send(&mut self, batch: Vec<Out>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        for (server, term, out) in coalesce(batch) {
            let reach = link.conns.get(&server).map(|c| &c.reach);
            if out == RemoteOut::Close {
                link.terms.open.remove(&term);
                if reach != Some(&Reach::Connected) {
                    link.terms.closed_away.push((server, term));
                    continue;
                }
            }
            // Typed while the server is away: lost, as it would be over SSH.
            if reach == Some(&Reach::Connected)
                && let Some(conn) = link.conns.get(&server)
            {
                conn.client.send(&message(&term, &out));
            }
        }
    }

    /// Server `server` is connected again: its terminals pick up where they
    /// were, and those closed meanwhile are hung up.
    pub(super) fn server_terms_back(&mut self, server: &str, cx: &App) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let Some(conn) = link.conns.get(server) else {
            return;
        };
        let terms = &mut link.terms;
        terms.open.retain(|_, t| t.terminal.upgrade().is_some());
        for (term, t) in terms.open.iter().filter(|(_, t)| t.server == server) {
            let Some(from) = t
                .terminal
                .upgrade()
                .and_then(|e| e.read(cx).remote_offset())
            else {
                continue;
            };
            conn.client
                .send(&json!({ "type": "term_attach", "term": term, "from": from }));
        }
        terms.closed_away.retain(|(s, term)| {
            if s != server {
                return true;
            }
            conn.client
                .send(&json!({ "type": "term_close", "term": term }));
            false
        });
    }

    /// A message about a terminal, or false when `msg` is not one.
    pub(super) fn server_term_message(
        &mut self,
        server: &str,
        msg: &Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = msg["type"].as_str().unwrap_or("");
        let message = text(msg, "message");
        let refused = kind == "error" && older_server_refused(&message, &["term_open"]);
        let failed = kind == "error" && msg["about"] == "term_open";
        // What follows an open an older server refused: said once, for the open.
        if kind == "error" && older_server_refused(&message, &TERM_KINDS) {
            return true;
        }
        if !kind.starts_with("term_") && !refused && !failed {
            return false;
        }
        if refused || failed {
            let body = if refused {
                "This server cannot open terminals yet. Update arakun-server on it and try again."
                    .to_string()
            } else {
                text(msg, "message")
            };
            self.notify(
                crate::island::AlertKind::Failure,
                "Could not open a terminal",
                &body,
                None,
                cx,
            );
            // An older server says no to each one it was asked for.
            if refused {
                let waiting: Vec<String> = self
                    .servers
                    .as_ref()
                    .map(|l| {
                        l.terms
                            .open
                            .iter()
                            .filter(|(_, t)| t.server == server)
                            .map(|(id, _)| id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for term in waiting {
                    self.server_term_ended(&term, None, cx);
                }
            }
            return true;
        }
        let term = text(msg, "term");
        match kind {
            "term_output" => {
                let Some(terminal) = self.server_terminal(server, &term) else {
                    return true;
                };
                let (Some(at), Ok(bytes)) = (msg["at"].as_u64(), B64.decode(text(msg, "data")))
                else {
                    return true;
                };
                let drawn = terminal.update(cx, |t, cx| t.feed(at, &bytes, cx));
                self.server_send_raw(
                    server,
                    &json!({ "type": "term_seen", "term": term, "at": drawn }),
                );
            }
            "term_exit" if self.server_terminal(server, &term).is_some() => {
                let code = msg["code"].as_i64().map(|c| c as i32);
                self.server_term_ended(&term, code, cx);
            }
            _ => {}
        }
        true
    }

    /// A `term_open` the connection dropped before it went: the terminal
    /// never started.
    pub(super) fn server_term_lost(&mut self, msg: &Value, cx: &mut Context<Self>) {
        self.notify(
            crate::island::AlertKind::Warning,
            "Could not open a terminal",
            "The connection to the server dropped first. Try again once it is back.",
            None,
            cx,
        );
        self.server_term_ended(&text(msg, "term"), None, cx);
    }

    fn server_terminal(&self, server: &str, term: &str) -> Option<Entity<Terminal>> {
        let t = self.servers.as_ref()?.terms.open.get(term)?;
        (t.server == server).then(|| t.terminal.upgrade()).flatten()
    }

    /// The terminal ended there: its tab closes, as a local one's does.
    fn server_term_ended(&mut self, term: &str, code: Option<i32>, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let Some(t) = link.terms.open.remove(term) else {
            return;
        };
        if let Some(terminal) = t.terminal.upgrade() {
            terminal.update(cx, |t, cx| t.remote_exited(code, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(server: &str, term: &str, out: RemoteOut) -> Out {
        (server.into(), term.into(), out)
    }

    #[test]
    fn keys_typed_in_a_row_go_out_together() {
        let batch = vec![
            out("s", "a", RemoteOut::Input(b"l".to_vec())),
            out("s", "a", RemoteOut::Input(b"s".to_vec())),
            out("s", "b", RemoteOut::Input(b"x".to_vec())),
            out("s", "a", RemoteOut::Resize { cols: 90, rows: 30 }),
            out("s", "a", RemoteOut::Input(b"\r".to_vec())),
        ];
        assert_eq!(
            coalesce(batch),
            vec![
                out("s", "a", RemoteOut::Input(b"ls".to_vec())),
                out("s", "b", RemoteOut::Input(b"x".to_vec())),
                out("s", "a", RemoteOut::Resize { cols: 90, rows: 30 }),
                out("s", "a", RemoteOut::Input(b"\r".to_vec())),
            ]
        );
    }

    #[test]
    fn what_a_terminal_says_is_the_server_s_message() {
        let input = message("t1", &RemoteOut::Input(b"hi".to_vec()));
        assert_eq!(input["type"], "term_input");
        assert_eq!(B64.decode(text(&input, "data")).unwrap(), b"hi");
        let close = message("t1", &RemoteOut::Close);
        assert_eq!(close, json!({ "type": "term_close", "term": "t1" }));
    }
}
