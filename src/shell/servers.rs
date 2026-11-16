//! The servers' side of the shell: the connections to the machines running
//! `arakun server` (`crate::servers`), what each said it holds, and the one
//! session on show in the Servers tab (`servers_view`).
//!
//! A server's session is not one of the shell's own: its agent runs over
//! there. The tab shows it through a thread that only holds a transcript, as a
//! headless session's does, fed with the entries the server sends whole
//! (`transcript`); what the user writes or answers goes back as the phone's
//! messages. Nothing connects until the tab is opened once.

use std::collections::HashMap;
use std::path::PathBuf;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{AppContext as _, Context, Entity, Task, Window};
use serde_json::{Value, json};

use super::Shell;
use crate::agent::{
    AgentKind, Entry, SessionMeta, Thread, ThreadStatus, ThreadView, Transcript, Usage,
};
use crate::model::{Status, TabKind, unix_now};
use crate::servers::client::{Client, Event, Events, Status as LinkStatus};
use crate::servers::{self, Saved};
use crate::servers_view::{
    Open, Permission, PlaceRow, Question, Reach, ServerRow, ServersEvent, ServersState, SessionRow,
};

/// One server: its connection and what it last said it holds.
struct Conn {
    client: Client,
    reach: Reach,
    sessions: Vec<SessionRow>,
    places: Vec<PlaceRow>,
}

impl Conn {
    fn new(client: Client) -> Conn {
        Conn {
            client,
            reach: Reach::Connecting,
            sessions: Vec::new(),
            places: Vec::new(),
        }
    }
}

/// The session on show, and how much of it has arrived.
struct Showing {
    server: String,
    session: String,
    thread: Entity<Thread>,
    view: Entity<ThreadView>,
    /// Its entries so far, whole, as the server's transcript holds them.
    entries: Vec<Entry>,
    agent: AgentKind,
    cwd: PathBuf,
    title: String,
    agent_name: String,
    place: String,
    status: Status,
    permission: Option<Permission>,
    questions: Option<(String, Vec<Question>)>,
    /// A `transcript` was asked for and has not come back.
    asking: bool,
    /// The index the one that is out asked from.
    asked: usize,
    /// Entries changed from this index on while one was out: asked for next.
    again: Option<usize>,
    /// The whole session being read again, from its start, into here: on
    /// opening it and after each reconnect. It takes the place of `entries`
    /// only once all of it is here, so the view never falls back to a first
    /// page and fills up again.
    refill: Option<Vec<Entry>>,
    /// Everything the server had when the session was opened is here.
    loaded: bool,
    /// The server's entries could not be read: nothing more is asked for.
    broken: bool,
}

pub(super) struct ServersLink {
    saved: Vec<Saved>,
    conns: HashMap<String, Conn>,
    /// Where every connection's thread tells how it goes.
    events: Events,
    showing: Option<Showing>,
    /// The id a pairing under way will carry once it works.
    pairing: Option<String>,
    /// Gives a pairing up when its link has run out.
    pairing_timeout: Option<Task<()>>,
    error: Option<String>,
    _listen: Task<()>,
}

fn status_from(word: &str) -> Status {
    match word {
        "working" => Status::Working,
        "needs_input" => Status::NeedsInput,
        "error" => Status::Error,
        _ => Status::Idle,
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn permission_from(v: &Value) -> Option<Permission> {
    let options = v["options"].as_array()?;
    Some(Permission {
        title: text(v, "title"),
        options: options
            .iter()
            .map(|o| (text(o, "id"), text(o, "name"), text(o, "kind")))
            .collect(),
    })
}

fn questions_from(v: &Value) -> Option<(String, Vec<Question>)> {
    let questions = v["questions"].as_array()?;
    Some((
        text(v, "tool_call_id"),
        questions
            .iter()
            .map(|q| Question {
                question: text(q, "question"),
                options: q["options"]
                    .as_array()
                    .map(|o| {
                        o.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect(),
    ))
}

/// The transcript a viewer thread is handed: the entries so far, as this session.
fn transcript_of(s: &Showing) -> Transcript {
    Transcript {
        version: 1,
        meta: SessionMeta {
            agent: s.agent,
            session_id: None,
            title: s.title.clone(),
            cwd: s.cwd.clone(),
            created: unix_now(),
            updated: unix_now(),
            pr: None,
        },
        entries: s.entries.clone(),
        usage: Usage::default(),
        fork_point: None,
        interrupted: false,
        resume: false,
        queue: Vec::new(),
        handoff: None,
        subagents: Vec::new(),
    }
}

/// What the viewer shows as the session's state. A failed turn left its
/// reason in the transcript, so the error needs no words of its own here.
fn thread_status(status: Status) -> ThreadStatus {
    match status {
        Status::Working => ThreadStatus::Working,
        Status::NeedsInput => ThreadStatus::NeedsInput,
        Status::Idle | Status::Error => ThreadStatus::Idle,
    }
}

/// A link is good for two minutes; a pairing still under way a little after
/// that will not finish.
const PAIRING_WAIT: std::time::Duration = std::time::Duration::from_secs(150);

/// A `transcript` answer: entries `from..to` of the session's `count`.
struct Page {
    from: usize,
    to: usize,
    count: usize,
    entries: Vec<Entry>,
}

/// Reads a `transcript` answer. An entry this app does not know (a server one
/// version ahead with a new kind) fails the whole page: half a transcript
/// with holes in it would be shown as if it were the session.
fn page_of(msg: &Value) -> Result<Page, String> {
    let number = |key: &str| msg[key].as_u64().unwrap_or(0) as usize;
    let entries = serde_json::from_value(msg["entries"].clone()).map_err(|e| e.to_string())?;
    Ok(Page {
        from: number("from"),
        to: number("to"),
        count: number("count"),
        entries,
    })
}

/// What to do after a page was put in.
#[derive(Debug, PartialEq)]
enum Next {
    /// All of it is here.
    Done,
    /// More is out: ask from this index.
    Ask(usize),
    /// The server answered with nothing new where more was due. Asking again
    /// would get the same answer, forever and as fast as the network goes.
    Stuck,
}

/// Puts a page into the entries so far. `asked` is the index it was asked from.
fn merge(entries: &mut Vec<Entry>, page: Page, asked: usize) -> Next {
    if page.from > entries.len() {
        // A gap: what is missing comes first, unless that was just asked for.
        return if asked == entries.len() {
            Next::Stuck
        } else {
            Next::Ask(entries.len())
        };
    }
    let brought = !page.entries.is_empty();
    entries.truncate(page.from);
    entries.extend(page.entries);
    if page.to >= page.count || entries.len() >= page.count {
        Next::Done
    } else if brought {
        Next::Ask(entries.len())
    } else {
        Next::Stuck
    }
}

impl Shell {
    /// Opens (or focuses) the Servers tab, and connects to the paired servers
    /// the first time.
    pub(super) fn open_servers_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.has_projects() {
            self.show_view_tab("Servers", TabKind::Servers, |k| {
                matches!(k, TabKind::Servers)
            });
            self.note_active_tab();
        } else {
            // Following servers needs no project here: the tab shows in the
            // welcome's place, as the GitHub tab does.
            self.servers_alone = true;
        }
        self.servers_connect(window, cx);
        self.sync_servers_view(cx);
        self.servers_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    fn servers_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.servers.is_some() {
            return;
        }
        let (events, mut heard) = mpsc::unbounded::<(String, Event)>();
        let listen = cx.spawn_in(window, async move |this, cx| {
            while let Some((id, event)) = heard.next().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.servers_heard(&id, event, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let saved = servers::load();
        let conns = saved
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    Conn::new(Client::connect(s.clone(), events.clone())),
                )
            })
            .collect();
        self.servers = Some(ServersLink {
            saved,
            conns,
            events,
            showing: None,
            pairing: None,
            pairing_timeout: None,
            error: None,
            _listen: listen,
        });
    }

    pub(super) fn sync_servers_view(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &self.servers else {
            return;
        };
        let servers = link
            .saved
            .iter()
            .filter_map(|s| {
                let conn = link.conns.get(&s.id)?;
                Some(ServerRow {
                    id: s.id.clone(),
                    name: if s.name.is_empty() {
                        s.origin.clone()
                    } else {
                        s.name.clone()
                    },
                    origin: s.origin.clone(),
                    reach: conn.reach.clone(),
                    sessions: conn.sessions.clone(),
                    places: conn.places.clone(),
                })
            })
            .collect();
        let open = link.showing.as_ref().map(|s| Open {
            server: s.server.clone(),
            session: s.session.clone(),
            title: s.title.clone(),
            agent: s.agent_name.clone(),
            place: s.place.clone(),
            status: s.status,
            permission: s.permission.clone(),
            questions: s.questions.clone(),
            view: s.view.clone(),
            loading: !s.loaded,
        });
        let state = ServersState {
            servers,
            open,
            pairing: link.pairing.is_some(),
            error: link.error.clone(),
        };
        self.servers_view.update(cx, |v, cx| v.set_state(state, cx));
    }

    /// A message to the server showing the open session, about that session.
    fn servers_tell(&self, mut msg: Value) {
        let Some(link) = &self.servers else {
            return;
        };
        let Some(showing) = &link.showing else {
            return;
        };
        msg["session"] = json!(showing.session);
        if let Some(conn) = link.conns.get(&showing.server) {
            conn.client.send(&msg);
        }
    }

    pub(super) fn on_servers_event(
        &mut self,
        ev: &ServersEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.servers_connect(window, cx);
        let Some(link) = &mut self.servers else {
            return;
        };
        link.error = None;
        match ev {
            ServersEvent::Pair(text) => match servers::parse_link(text) {
                Ok(invite) => {
                    let id = servers::new_id();
                    let client = Client::pair(
                        id.clone(),
                        invite,
                        servers::device_name(),
                        link.events.clone(),
                    );
                    // A pairing still under way is dropped, which hangs it up.
                    if let Some(before) = link.pairing.take() {
                        link.conns.remove(&before);
                    }
                    link.conns.insert(id.clone(), Conn::new(client));
                    link.pairing = Some(id.clone());
                    link.pairing_timeout = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(PAIRING_WAIT).await;
                        let _ = this.update(cx, |this, cx| this.servers_pairing_ran_out(&id, cx));
                    }));
                }
                Err(e) => link.error = Some(e),
            },
            ServersEvent::CancelPairing => {
                if let Some(id) = link.pairing.take() {
                    link.conns.remove(&id);
                }
                link.pairing_timeout = None;
            }
            ServersEvent::Forget(id) => {
                if let Some(conn) = link.conns.remove(id) {
                    conn.client.unpair();
                }
                link.saved.retain(|s| s.id != *id);
                servers::save(&link.saved);
                if link.showing.as_ref().is_some_and(|s| s.server == *id) {
                    link.showing = None;
                }
            }
            ServersEvent::Open { server, session } => {
                self.servers_show(server, session, window, cx);
            }
            ServersEvent::Start {
                server,
                place,
                agent,
                text,
            } => {
                if let Some(conn) = link.conns.get(server) {
                    conn.client.send(&json!({
                        "type": "start", "env": place, "agent": agent, "text": text, "settings": [],
                    }));
                }
            }
            ServersEvent::Send(text) => {
                self.servers_tell(json!({ "type": "send", "text": text, "uploads": [] }))
            }
            ServersEvent::Stop => self.servers_tell(json!({ "type": "cancel" })),
            ServersEvent::Permission(option) => {
                self.servers_tell(json!({ "type": "permission", "option": option }))
            }
            ServersEvent::Answer(answers) => {
                self.servers_tell(json!({ "type": "answer", "answers": answers }))
            }
            ServersEvent::SkipQuestion => self.servers_tell(json!({ "type": "skip_question" })),
        }
        self.sync_servers_view(cx);
    }

    /// The link of a pairing still under way has run out: it is given up.
    fn servers_pairing_ran_out(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        if link.pairing.as_deref() != Some(id) {
            return;
        }
        link.pairing = None;
        link.conns.remove(id);
        link.error = Some("Pairing took too long; ask the server for a new link.".into());
        self.sync_servers_view(cx);
    }

    /// The last Servers tab was closed: the server is told nobody is looking
    /// at the session any more, so it can let that session's agent go.
    pub(super) fn servers_tab_closed(&mut self, cx: &mut Context<Self>) {
        let still_open = self.servers_alone
            || self
                .projects
                .iter()
                .flat_map(|p| &p.environments)
                .flat_map(|e| &e.tabs)
                .any(|t| matches!(t.kind, TabKind::Servers));
        let Some(link) = &mut self.servers else {
            return;
        };
        if still_open {
            return;
        }
        if let Some(showing) = link.showing.take()
            && let Some(conn) = link.conns.get(&showing.server)
        {
            conn.client.send(&json!({ "type": "close" }));
        }
        // What went wrong was about what was on show.
        link.error = None;
        self.sync_servers_view(cx);
    }

    /// Puts a server's session on show and asks the server for it.
    fn servers_show(
        &mut self,
        server: &str,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        if link
            .showing
            .as_ref()
            .is_some_and(|s| s.server == server && s.session == session)
        {
            return;
        }
        let Some(conn) = link.conns.get(server) else {
            return;
        };
        // Nothing would answer: say so instead of waiting on it.
        if conn.reach != Reach::Connected {
            link.error =
                Some("That server is not connected; its sessions cannot be read right now.".into());
            return;
        }
        let row = conn.sessions.iter().find(|s| s.id == session);
        let (title, agent_name, status) = row
            .map_or((String::new(), String::new(), Status::Idle), |r| {
                (r.title.clone(), r.agent.clone(), r.status)
            });
        let agent = AgentKind::parse(&agent_name).unwrap_or(AgentKind::Claude);
        // The server stops sending the last one's changes once it hears of another.
        let before = link.showing.take();
        if let Some(before) = before.filter(|b| b.server != server)
            && let Some(other) = link.conns.get(&before.server)
        {
            other.client.send(&json!({ "type": "close" }));
        }
        // Empty until its entries come; the status shows it is on its way.
        let thread = Thread::viewer(empty_transcript(agent, &title), thread_status(status), cx);
        let view = cx.new(|cx| ThreadView::new(thread.clone(), window, cx));
        let showing = Showing {
            server: server.to_string(),
            session: session.to_string(),
            thread,
            view,
            entries: Vec::new(),
            agent,
            cwd: PathBuf::new(),
            title,
            agent_name,
            place: String::new(),
            status,
            permission: None,
            questions: None,
            asking: true,
            asked: 0,
            again: None,
            refill: Some(Vec::new()),
            loaded: false,
            broken: false,
        };
        conn.client
            .send(&json!({ "type": "open", "session": session }));
        conn.client
            .send(&json!({ "type": "transcript", "session": session, "from": 0 }));
        link.showing = Some(showing);
        self.servers_view.update(cx, |v, cx| v.show_session(cx));
    }

    /// What a connection's thread says: how it stands, or a message from the server.
    fn servers_heard(
        &mut self,
        id: &str,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        match event {
            Event::Paired(saved) => {
                // A pairing given up (cancelled, or another link pasted over
                // it) may still finish on its way out. Its connection is gone,
                // so it would sit in the list as a server nothing talks to,
                // and take the pairing now under way for done.
                if link.pairing.as_deref() != Some(id) || !link.conns.contains_key(id) {
                    return;
                }
                link.pairing = None;
                link.pairing_timeout = None;
                // "The server is not at the relay" was about the wait that just ended.
                link.error = None;
                link.saved.push(saved);
                servers::save(&link.saved);
            }
            Event::Status(status) => {
                let pairing = link.pairing.as_deref() == Some(id);
                let reach = match status {
                    LinkStatus::Connecting => Reach::Connecting,
                    LinkStatus::Connected => Reach::Connected,
                    LinkStatus::ServerAway => {
                        // Mid-pairing this is why nothing happens: say it. The
                        // pairing goes on if the server is back before the link runs out.
                        if pairing {
                            link.error = Some(
                                "The server is not at the relay right now. Pairing goes on if it comes back while the link lasts.".into(),
                            );
                        }
                        Reach::Away("The server is not at the relay right now.".into())
                    }
                    LinkStatus::Waiting { why, .. } => Reach::Away(why),
                    // A link that did not work leaves nothing to keep.
                    LinkStatus::Refused(why) if pairing => {
                        link.pairing = None;
                        link.pairing_timeout = None;
                        link.conns.remove(id);
                        link.error = Some(why);
                        self.sync_servers_view(cx);
                        return;
                    }
                    LinkStatus::Refused(why) => Reach::Refused(why),
                };
                let back = reach == Reach::Connected;
                if let Some(conn) = link.conns.get_mut(id) {
                    conn.reach = reach;
                    // Back after a drop: the open session is asked for again,
                    // whole, since what changed meanwhile was not heard. It is
                    // read to the side and swapped in when complete.
                    if back && let Some(showing) = link.showing.as_mut().filter(|s| s.server == id)
                    {
                        (showing.asking, showing.asked, showing.again) = (true, 0, None);
                        (showing.refill, showing.broken) = (Some(Vec::new()), false);
                        conn.client
                            .send(&json!({ "type": "open", "session": showing.session }));
                        conn.client.send(
                            &json!({ "type": "transcript", "session": showing.session, "from": 0 }),
                        );
                    }
                }
            }
            Event::Message(msg) => {
                if msg["type"] == "started" {
                    let session = text(&msg, "id");
                    self.servers_show(id, &session, window, cx);
                } else {
                    self.servers_message(id, &msg, cx);
                }
            }
        }
        self.sync_servers_view(cx);
    }

    fn servers_message(&mut self, id: &str, msg: &Value, cx: &mut Context<Self>) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let Some(conn) = link.conns.get_mut(id) else {
            return;
        };
        let showing = link
            .showing
            .as_mut()
            .filter(|s| s.server == id && msg["id"] == s.session.as_str());
        match msg["type"].as_str().unwrap_or("") {
            "inbox" => {
                let rows = |key: &str| msg[key].as_array().cloned().unwrap_or_default();
                conn.sessions = rows("sessions")
                    .iter()
                    .map(|s| SessionRow {
                        id: text(s, "id"),
                        title: text(s, "title"),
                        agent: text(s, "agent"),
                        status: status_from(s["status"].as_str().unwrap_or("")),
                    })
                    .collect();
                conn.places = rows("places")
                    .iter()
                    .map(|p| PlaceRow {
                        path: text(p, "path"),
                        project: text(p, "project"),
                        branch: text(p, "branch"),
                        agent: text(p, "agent"),
                    })
                    .collect();
                // A session the server forgot is not on show any more.
                if link.showing.as_ref().is_some_and(|s| {
                    s.server == id && !conn.sessions.iter().any(|r| r.id == s.session)
                }) {
                    link.showing = None;
                }
            }
            "session" | "state" => {
                let Some(showing) = showing else { return };
                showing.status = status_from(msg["status"].as_str().unwrap_or(""));
                showing.permission = permission_from(&msg["permission"]);
                showing.questions = questions_from(&msg["question"]);
                if let Some(title) = msg["title"].as_str().filter(|t| !t.is_empty()) {
                    showing.title = title.to_string();
                }
                if msg["type"] == "session" {
                    showing.agent_name = text(msg, "agent");
                    showing.place = text(msg, "place");
                }
                let (thread, status) = (showing.thread.clone(), thread_status(showing.status));
                let transcript = transcript_of(showing);
                thread.update(cx, |t, cx| t.show_transcript(transcript, status, cx));
            }
            // Entries changed, cut down for a phone: the whole ones are asked for.
            "entries" => {
                let Some(showing) = showing else { return };
                if msg["older"] == true || showing.broken {
                    return;
                }
                let from = msg["from"].as_u64().unwrap_or(0) as usize;
                if showing.asking {
                    showing.again = Some(showing.again.map_or(from, |a| a.min(from)));
                } else {
                    (showing.asking, showing.asked) = (true, from);
                    conn.client.send(
                        &json!({ "type": "transcript", "session": showing.session, "from": from }),
                    );
                }
            }
            "transcript" => {
                let Some(showing) = showing else { return };
                if showing.broken {
                    return;
                }
                if let Some(agent) = AgentKind::parse(msg["agent"].as_str().unwrap_or("")) {
                    showing.agent = agent;
                }
                showing.cwd = PathBuf::from(text(msg, "cwd"));
                let asked = showing.asked;
                let next = page_of(msg).map(|page| match showing.refill.as_mut() {
                    Some(refill) => merge(refill, page, asked),
                    None => merge(&mut showing.entries, page, asked),
                });
                let ask = match next {
                    Ok(Next::Ask(from)) => Some(from),
                    Ok(Next::Done) => {
                        // Read whole: now it takes the place of what was shown.
                        if let Some(whole) = showing.refill.take() {
                            showing.entries = whole;
                        }
                        showing.loaded = true;
                        // What changed while this one was out.
                        showing.again.take()
                    }
                    // Asking again would bring the same answer. What is here
                    // stays on show, and the user is told why it stops there.
                    failed => {
                        (showing.broken, showing.asking, showing.again) = (true, false, None);
                        (showing.refill, showing.loaded) = (None, true);
                        link.error = Some(match failed {
                            // The reason without serde's list of every kind it knows.
                            Err(e) => format!(
                                "A newer server? This session holds something this app cannot read ({}). Update this app to follow it.",
                                e.split(", expected").next().unwrap_or(&e)
                            ),
                            _ => "The server stopped sending this session part way; open it again to retry.".into(),
                        });
                        None
                    }
                };
                match ask {
                    Some(from) => {
                        showing.asked = from;
                        conn.client.send(
                            &json!({ "type": "transcript", "session": showing.session, "from": from }),
                        );
                    }
                    None => showing.asking = false,
                }
                // A refill shows once it is whole; what is on screen stays until then.
                if showing.refill.is_none() {
                    let (thread, status) = (showing.thread.clone(), thread_status(showing.status));
                    let transcript = transcript_of(showing);
                    thread.update(cx, |t, cx| t.show_transcript(transcript, status, cx));
                }
            }
            "error" => link.error = Some(text(msg, "message")),
            _ => {}
        }
    }
}

/// An empty transcript for a session that is about to be read.
fn empty_transcript(agent: AgentKind, title: &str) -> Transcript {
    Transcript {
        version: 1,
        meta: SessionMeta {
            agent,
            session_id: None,
            title: title.to_string(),
            cwd: PathBuf::new(),
            created: unix_now(),
            updated: unix_now(),
            pr: None,
        },
        entries: Vec::new(),
        usage: Usage::default(),
        fork_point: None,
        interrupted: false,
        resume: false,
        queue: Vec::new(),
        handoff: None,
        subagents: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Entry {
        Entry::Agent { text: text.into() }
    }

    fn texts(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Agent { text } => text.as_str(),
                _ => "?",
            })
            .collect()
    }

    fn page(from: usize, to: usize, count: usize, new: &[&str]) -> Page {
        Page {
            from,
            to,
            count,
            entries: new.iter().map(|t| said(t)).collect(),
        }
    }

    #[test]
    fn a_transcript_arrives_in_answers_and_follows_what_changes() {
        let mut entries = Vec::new();
        // The first answer held two of five: the rest is asked for from there.
        assert_eq!(
            merge(&mut entries, page(0, 2, 5, &["a", "b"]), 0),
            Next::Ask(2)
        );
        assert_eq!(
            merge(&mut entries, page(2, 5, 5, &["c", "d", "e"]), 2),
            Next::Done
        );
        assert_eq!(texts(&entries), ["a", "b", "c", "d", "e"]);
        // The last entry grew and one came after it: the tail is replaced.
        assert_eq!(
            merge(&mut entries, page(4, 6, 6, &["e!", "f"]), 4),
            Next::Done
        );
        assert_eq!(texts(&entries), ["a", "b", "c", "d", "e!", "f"]);
        // An answer that starts past what is here joins nothing: the gap first.
        assert_eq!(
            merge(&mut entries, page(9, 10, 10, &["j"]), 9),
            Next::Ask(6)
        );
        assert_eq!(entries.len(), 6);
        // The session was rewound on the server: fewer entries than before.
        assert_eq!(merge(&mut entries, page(1, 2, 2, &["b2"]), 1), Next::Done);
        assert_eq!(texts(&entries), ["a", "b2"]);
    }

    #[test]
    fn an_answer_that_brings_nothing_is_not_asked_for_again() {
        let mut entries = vec![said("a"), said("b")];
        // More is due, and the page is empty: the same question would get it again.
        assert_eq!(merge(&mut entries, page(2, 2, 5, &[]), 2), Next::Stuck);
        // The gap was asked for, and the answer still starts past it.
        assert_eq!(merge(&mut entries, page(4, 5, 5, &["e"]), 2), Next::Stuck);
        assert_eq!(texts(&entries), ["a", "b"]);
    }

    #[test]
    fn a_page_with_an_entry_this_app_does_not_know_fails_whole() {
        let good = json!({
            "type": "transcript", "id": "s1", "from": 3, "to": 5, "count": 9,
            "entries": [{ "type": "agent", "text": "a" }, { "type": "agent", "text": "b" }],
        });
        let read = page_of(&good).unwrap();
        assert_eq!((read.from, read.to, read.count), (3, 5, 9));
        assert_eq!(texts(&read.entries), ["a", "b"]);
        // A server one version ahead: never an empty page, which asks again forever.
        let newer = json!({
            "type": "transcript", "id": "s1", "from": 0, "to": 2, "count": 2,
            "entries": [{ "type": "agent", "text": "a" }, { "type": "hologram", "shape": "cube" }],
        });
        assert!(page_of(&newer).err().unwrap().contains("hologram"));
    }

    #[test]
    fn what_the_agent_asks_is_read_from_the_servers_words() {
        let p = permission_from(&json!({
            "tool_call_id": "t1", "title": "Run cargo test",
            "options": [
                { "id": "a", "name": "Allow", "kind": "allow_once" },
                { "id": "r", "name": "Reject", "kind": "reject_once" },
            ],
        }))
        .unwrap();
        assert_eq!(p.title, "Run cargo test");
        assert_eq!(
            p.options[1],
            ("r".into(), "Reject".into(), "reject_once".into())
        );
        assert!(permission_from(&Value::Null).is_none());

        let (id, questions) = questions_from(&json!({
            "tool_call_id": "q1",
            "questions": [{ "index": 1, "topic": "db", "question": "Which one?", "options": ["sqlite", "postgres"], "multi": false }],
        }))
        .unwrap();
        assert_eq!(id, "q1");
        assert_eq!(questions[0].question, "Which one?");
        assert_eq!(questions[0].options, ["sqlite", "postgres"]);
        assert!(questions_from(&Value::Null).is_none());

        assert_eq!(status_from("needs_input"), Status::NeedsInput);
        assert_eq!(status_from("something new"), Status::Idle);
    }
}
