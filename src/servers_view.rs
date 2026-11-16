//! The Servers tab: the machines running `arakun server` this app is paired
//! with, their sessions, and one of them open as the app shows its own. It
//! pairs with the link the server prints, starts sessions in the folders the
//! server offers, sends follow-ups and answers what the agents ask. The shell
//! keeps the connections and tells this view what there is (`shell::servers`).

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::agent::ThreadView;
use crate::input::{InputEvent, TextInput};
use crate::model::Status;
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

pub enum ServersEvent {
    /// Pair with the server this link is for.
    Pair(String),
    /// Give up the pairing under way.
    CancelPairing,
    /// Forget a server; it is told to forget this app too.
    Forget(String),
    /// Show one of a server's sessions.
    Open {
        server: String,
        session: String,
    },
    /// A new session on a server: the folder (its path there), the agent's key
    /// and the first prompt.
    Start {
        server: String,
        place: String,
        agent: String,
        text: String,
    },
    /// The rest act on the open session.
    Send(String),
    Stop,
    /// Answers the permission the agent asks for with one of its options.
    Permission(String),
    /// One answer for each question the agent asked.
    Answer(Vec<String>),
    SkipQuestion,
}

/// How the connection to a server stands.
#[derive(Clone, PartialEq)]
pub enum Reach {
    Connecting,
    Connected,
    /// Not reachable for now, and why; it is tried again by itself.
    Away(String),
    /// The server will not have this app; pairing again is the way back.
    Refused(String),
}

#[derive(Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub agent: String,
    pub status: Status,
}

/// A folder on the server where a session can start.
#[derive(Clone, PartialEq)]
pub struct PlaceRow {
    pub path: String,
    pub project: String,
    pub branch: String,
    /// The key of the agent sessions start with there.
    pub agent: String,
}

#[derive(Clone, PartialEq)]
pub struct ServerRow {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub reach: Reach,
    pub sessions: Vec<SessionRow>,
    pub places: Vec<PlaceRow>,
}

/// A permission the open session's agent waits on.
#[derive(Clone, PartialEq)]
pub struct Permission {
    pub title: String,
    /// Each option's id, name and kind (`allow_once`, `reject_once`...).
    pub options: Vec<(String, String, String)>,
}

#[derive(Clone, PartialEq)]
pub struct Question {
    pub question: String,
    pub options: Vec<String>,
}

/// The session on show.
#[derive(Clone, PartialEq)]
pub struct Open {
    pub server: String,
    pub session: String,
    pub title: String,
    pub agent: String,
    pub place: String,
    pub status: Status,
    pub permission: Option<Permission>,
    /// The questions the agent asked, under the id of the call that asked them.
    pub questions: Option<(String, Vec<Question>)>,
    /// The transcript, drawn as the app draws its own sessions.
    pub view: Entity<ThreadView>,
    /// Its entries are still on their way.
    pub loading: bool,
    /// Why the session could not be read in full, when it could not.
    pub problem: Option<String>,
}

#[derive(Clone, PartialEq, Default)]
pub struct ServersState {
    pub servers: Vec<ServerRow>,
    pub open: Option<Open>,
    /// A pairing is under way.
    pub pairing: bool,
    /// What went wrong last, until something else happens.
    pub error: Option<String>,
}

/// What the side beside the list shows.
#[derive(Clone, PartialEq)]
enum Pane {
    /// The open session, or a word about picking one.
    Session,
    /// The form to pair with a server.
    Add,
    /// The form for a new session on this server.
    New(String),
}

/// The agents a session can start with, by key.
const AGENTS: [(&str, &str); 4] = [
    ("claude", "Claude Code"),
    ("droid", "Droid"),
    ("opencode", "OpenCode"),
    ("workers-ai", "Workers AI"),
];

/// The list of servers, as wide as a sidebar.
const LIST_W: f32 = 260.;

pub struct ServersView {
    state: ServersState,
    pane: Pane,
    link: Entity<TextInput>,
    /// The follow-up to the open session.
    prompt: Entity<TextInput>,
    /// Follow-ups typed and not sent, by server and session. The field shows
    /// the open session's only: text typed for one must not go to another.
    drafts: HashMap<(String, String), String>,
    /// The first prompt of a new session.
    first: Entity<TextInput>,
    /// A typed answer to the agent's question.
    answer: Entity<TextInput>,
    /// The folder and agent picked for the new session, when not the first offered.
    place: Option<String>,
    agent: Option<String>,
    /// Answers given so far to the questions under this key: the server, the
    /// session and the id of the call that asked.
    answers: (String, Vec<String>),
    /// "Forget" was pressed once on this server; the next press counts.
    confirm_forget: Option<String>,
    focus: FocusHandle,
    _inputs: Vec<Subscription>,
}

impl EventEmitter<ServersEvent> for ServersView {}

fn tone(status: Status) -> (Tone, &'static str) {
    match status {
        Status::Working => (Tone::Info, "working"),
        Status::NeedsInput => (Tone::Warning, "needs you"),
        Status::Error => (Tone::Danger, "failed"),
        Status::Idle => (Tone::Neutral, "idle"),
    }
}

impl ServersView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| TextInput::new("https://…", cx).mono());
        let prompt = cx.new(|cx| TextInput::new("Send a follow-up…", cx));
        let first = cx.new(|cx| TextInput::new("What should the agent do?", cx));
        let answer = cx.new(|cx| TextInput::new("Or type an answer…", cx));
        let on_submit = |cx: &mut Context<Self>,
                         input: &Entity<TextInput>,
                         act: fn(&mut Self, &mut Context<Self>)| {
            cx.subscribe(input, move |this, _, ev: &InputEvent, cx| {
                if *ev == InputEvent::Submit {
                    act(this, cx);
                }
            })
        };
        let inputs = vec![
            on_submit(cx, &link, Self::pair),
            on_submit(cx, &prompt, Self::send),
            on_submit(cx, &first, Self::start),
            on_submit(cx, &answer, Self::answer_typed),
        ];
        Self {
            state: ServersState::default(),
            pane: Pane::Add,
            link,
            prompt,
            drafts: HashMap::new(),
            first,
            answer,
            place: None,
            agent: None,
            answers: (String::new(), Vec::new()),
            confirm_forget: None,
            focus: cx.focus_handle(),
            _inputs: inputs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn set_state(&mut self, state: ServersState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        // Paired: the link did its work, and the new server has the floor.
        if state.servers.len() > self.state.servers.len() && self.pane == Pane::Add {
            self.link.update(cx, |i, cx| i.set_text("", cx));
            self.pane = Pane::Session;
        }
        if state.servers.is_empty() {
            self.pane = Pane::Add;
        }
        if let Pane::New(id) = &self.pane
            && !state.servers.iter().any(|s| s.id == *id)
        {
            self.pane = Pane::Session;
        }
        // Another question, or the same id asked in another session: the
        // answers so far, and what is typed in the field, were to the last one.
        let asked = state
            .open
            .as_ref()
            .and_then(|o| Some((o, o.questions.as_ref()?)))
            .map(|(o, (id, _))| format!("{}\n{}\n{id}", o.server, o.session))
            .unwrap_or_default();
        if self.answers.0 != asked {
            self.answers = (asked, Vec::new());
            self.answer.update(cx, |i, cx| i.set_text("", cx));
        }
        // Another session on show: what was typed stays with the one it was for.
        let key =
            |open: &Option<Open>| open.as_ref().map(|o| (o.server.clone(), o.session.clone()));
        let (was, now) = (key(&self.state.open), key(&state.open));
        if was != now {
            let typed = self.prompt.read(cx).text().to_string();
            if let Some(was) = was.filter(|_| !typed.trim().is_empty()) {
                self.drafts.insert(was, typed);
            }
            let draft = now.and_then(|k| self.drafts.remove(&k)).unwrap_or_default();
            self.prompt.update(cx, |i, cx| i.set_text(draft, cx));
        }
        self.state = state;
        cx.notify();
    }

    /// Shows the open session: one was just picked or started.
    pub fn show_session(&mut self, cx: &mut Context<Self>) {
        self.pane = Pane::Session;
        cx.notify();
    }

    fn server(&self, id: &str) -> Option<&ServerRow> {
        self.state.servers.iter().find(|s| s.id == id)
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).text().trim().to_string();
        if !link.is_empty() && !self.state.pairing {
            cx.emit(ServersEvent::Pair(link));
        }
    }

    /// The open session's server can be told something right now. Without a
    /// connection nothing typed or picked is taken: it would be dropped on
    /// the way, and the field would look as if it had been sent.
    fn open_connected(&self) -> bool {
        self.state
            .open
            .as_ref()
            .and_then(|o| self.server(&o.server))
            .is_some_and(|s| s.reach == Reach::Connected)
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.prompt.read(cx).text().trim().to_string();
        if text.is_empty() || !self.open_connected() {
            return;
        }
        self.prompt.update(cx, |i, cx| i.set_text("", cx));
        cx.emit(ServersEvent::Send(text));
    }

    /// The folder a new session on `server` starts in: the one picked, or the first.
    fn picked_place<'a>(&self, server: &'a ServerRow) -> Option<&'a PlaceRow> {
        server
            .places
            .iter()
            .find(|p| Some(&p.path) == self.place.as_ref())
            .or(server.places.first())
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let Pane::New(id) = &self.pane else { return };
        let text = self.first.read(cx).text().trim().to_string();
        let Some(server) = self.server(id) else {
            return;
        };
        let Some(place) = self.picked_place(server) else {
            return;
        };
        if text.is_empty() || server.reach != Reach::Connected {
            return;
        }
        let event = ServersEvent::Start {
            server: server.id.clone(),
            place: place.path.clone(),
            agent: self.agent.clone().unwrap_or_else(|| place.agent.clone()),
            text,
        };
        self.first.update(cx, |i, cx| i.set_text("", cx));
        self.pane = Pane::Session;
        cx.emit(event);
        cx.notify();
    }

    /// One more answer; once every question has one, they go to the agent.
    fn give_answer(&mut self, answer: String, cx: &mut Context<Self>) {
        let Some((_, questions)) = self.state.open.as_ref().and_then(|o| o.questions.as_ref())
        else {
            return;
        };
        if !self.open_connected() {
            return;
        }
        self.answers.1.push(answer);
        if self.answers.1.len() >= questions.len() {
            cx.emit(ServersEvent::Answer(std::mem::take(&mut self.answers.1)));
        }
        cx.notify();
    }

    fn answer_typed(&mut self, cx: &mut Context<Self>) {
        let text = self.answer.read(cx).text().trim().to_string();
        if !text.is_empty() && self.open_connected() {
            self.answer.update(cx, |i, cx| i.set_text("", cx));
            self.give_answer(text, cx);
        }
    }

    fn label(text: impl Into<SharedString>) -> gpui::Div {
        ui::text(Type::Label, theme::text_subtle(), text)
    }

    fn code(text: impl Into<SharedString>) -> gpui::Div {
        div()
            .font_family(theme::FONT_MONO)
            .text_size(px(ui::TEXT_SM))
            .px(ui::sp(8.))
            .py(ui::sp(3.))
            .rounded(ui::rad(ui::RADIUS))
            .bg(theme::fill_raised())
            .text_color(theme::content())
            .child(text.into())
    }

    // ---- the list ------------------------------------------------------------

    fn render_server(&self, ix: usize, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let (dot, note) = match &server.reach {
            Reach::Connected => (Tone::Success, None),
            Reach::Connecting => (Tone::Neutral, Some("Connecting…".to_string())),
            Reach::Away(why) => (Tone::Warning, Some(why.clone())),
            Reach::Refused(why) => (Tone::Danger, Some(why.clone())),
        };
        let open = self.state.open.as_ref().filter(|o| o.server == server.id);
        let can_start = server.reach == Reach::Connected;
        let (new_id, forget_id) = (server.id.clone(), server.id.clone());
        let head = div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .h(px(ui::BUTTON_H))
            .child(
                div()
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .bg(dot.color()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .ty(Type::Label)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::content())
                    .child(server.name.clone()),
            )
            .child(
                ui::icon_button(("servers-forget", ix), "delete02", 13.)
                    .tooltip(ui::tip("Forget this server"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_forget = Some(forget_id.clone());
                        cx.notify();
                    })),
            )
            .when(can_start, |d| {
                d.child(
                    ui::icon_button(("servers-new", ix), "add01", 14.)
                        .tooltip(ui::tip("New session"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            (this.place, this.agent) = (None, None);
                            this.pane = Pane::New(new_id.clone());
                            cx.notify();
                        })),
                )
            });
        let confirm = (self.confirm_forget.as_deref() == Some(server.id.as_str())).then(|| {
            let id = server.id.clone();
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(6.))
                .px(ui::sp(8.))
                .pb(ui::sp(6.))
                .child(ui::text(
                    Type::Caption,
                    theme::attention().into(),
                    "Forget this server? Its sessions stay on it.",
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(6.))
                        .child(
                            ui::button(
                                ("servers-forget-yes", ix),
                                "Forget",
                                ButtonKind::Danger,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.confirm_forget = None;
                                    cx.emit(ServersEvent::Forget(id.clone()));
                                },
                            )),
                        )
                        .child(
                            ui::button(("servers-forget-no", ix), "Keep", ButtonKind::Ghost, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_forget = None;
                                    cx.notify();
                                })),
                        ),
                )
        });
        let sessions = server.sessions.iter().enumerate().map(|(i, s)| {
            let selected = open.is_some_and(|o| o.session == s.id) && self.pane == Pane::Session;
            let (tone, _) = tone(s.status);
            let (server_id, session_id) = (server.id.clone(), s.id.clone());
            div()
                .id(("servers-session", ix * 10_000 + i))
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .min_h(px(ui::MENU_ROW_H))
                .px(ui::sp(8.))
                .rounded(ui::rad(ui::RADIUS))
                .cursor_pointer()
                .hover(|d| d.bg(theme::fill_hover()))
                .when(selected, |d| d.bg(theme::fill_selected()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pane = Pane::Session;
                    cx.emit(ServersEvent::Open {
                        server: server_id.clone(),
                        session: session_id.clone(),
                    });
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(tone.color()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .ty(Type::Body)
                        .text_color(theme::text())
                        .child(s.title.clone()),
                )
                .child(ui::text(Type::Caption, theme::text_faint(), s.agent.clone()).flex_none())
        });
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(2.))
            .child(head)
            .children(note.map(|n| {
                ui::text(Type::Caption, theme::text_faint(), n)
                    .px(ui::sp(8.))
                    .pb(ui::sp(4.))
            }))
            .children(confirm)
            .children(sessions)
            .when(server.sessions.is_empty() && can_start, |d| {
                d.child(
                    ui::text(Type::Caption, theme::text_faint(), "No sessions yet.").px(ui::sp(8.)),
                )
            })
            .into_any_element()
    }

    // ---- pairing -------------------------------------------------------------

    fn render_add(&self, cx: &mut Context<Self>) -> AnyElement {
        let pairing = self.state.pairing;
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(ui::PAGE_PAD))
            .max_w(px(620.))
            .child(ui::page_title(
                "Pair with a server",
                Some(
                    "Run sessions on another machine, and follow them from here."
                        .into_any_element(),
                ),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(8.))
                    .child(Self::label("On that machine, start the server and ask it for a link:"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(ui::sp(8.))
                            .child(Self::code("arakun-server serve"))
                            .child(Self::code("arakun-server pair")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(8.))
                    .child(Self::label("Paste the link here. It is good for two minutes, and for one device:"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(
                                ui::field()
                                    .flex_1()
                                    .child(icon("link01", 14.).text_color(theme::text_subtle()))
                                    .child(self.link.clone()),
                            )
                            .child(
                                ui::button(
                                    "servers-pair",
                                    if pairing { "Pairing…" } else { "Pair" },
                                    ButtonKind::Primary,
                                    !pairing,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.pair(cx))),
                            )
                            // Through the relay a pairing can wait on the
                            // server for as long as the link lasts.
                            .when(pairing, |d| {
                                d.child(
                                    ui::button(
                                        "servers-pair-cancel",
                                        "Cancel",
                                        ButtonKind::Ghost,
                                        true,
                                    )
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ServersEvent::CancelPairing)
                                    })),
                                )
                            }),
                    ),
            )
            .children(self.state.error.clone().map(|e| {
                ui::text(Type::Label, theme::attention().into(), e)
            }))
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "Through the relay, everything between this app and the server is encrypted end to end. A link that starts with http:// is not encrypted: use one only on a network you trust. Agents there use that machine's logins and run in its folders.",
            ))
            .into_any_element()
    }

    // ---- a new session -------------------------------------------------------

    fn render_new(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let page = div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(ui::PAGE_PAD))
            .max_w(px(620.))
            .child(ui::page_title(
                format!("New session on {}", server.name),
                None,
            ));
        let Some(picked) = self.picked_place(server) else {
            return page
                .child(Self::label(
                    "The server has no folder to start a session in yet. On it, name one:",
                ))
                .child(div().flex().child(Self::code("arakun-server add <dir>")))
                .into_any_element();
        };
        let agent = self.agent.clone().unwrap_or_else(|| picked.agent.clone());
        let places = server.places.iter().enumerate().map(|(i, p)| {
            let path = p.path.clone();
            ui::choice_row(("servers-place", i), p.path == picked.path)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.place = Some(path.clone());
                    this.agent = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().truncate().child(if p.branch.is_empty() {
                            p.project.clone()
                        } else {
                            format!("{} · {}", p.project, p.branch)
                        }))
                        .child(
                            ui::text(Type::Caption, theme::text_faint(), p.path.clone()).truncate(),
                        ),
                )
        });
        let agents = AGENTS
            .iter()
            .enumerate()
            .fold(ui::segmented(), |track, (i, (key, name))| {
                track.child(
                    ui::segment(("servers-agent", i), None, *name, agent == *key).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.agent = Some(key.to_string());
                            cx.notify();
                        }),
                    ),
                )
            });
        let connected = server.reach == Reach::Connected;
        page.child(
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(6.))
                .child(Self::label("Folder"))
                .children(places),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(6.))
                .child(Self::label("Agent"))
                .child(div().flex().child(agents)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .child(ui::field().flex_1().child(self.first.clone()))
                .child(
                    ui::button("servers-start", "Start", ButtonKind::Primary, connected)
                        .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                ),
        )
        .when(!connected, |d| {
            d.child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "The server is not connected right now.",
            ))
        })
        .into_any_element()
    }

    // ---- the open session ----------------------------------------------------

    /// What the agent waits on the user for, with the ways to answer it.
    fn render_asks(&self, open: &Open, cx: &mut Context<Self>) -> Option<AnyElement> {
        let connected = self.open_connected();
        let band = || {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(ui::sp(8.))
                .px(ui::sp(12.))
                .py(ui::sp(10.))
                .border_t_1()
                .border_color(theme::border_subtle())
        };
        if let Some(p) = &open.permission {
            let options = p.options.iter().enumerate().map(|(i, (id, name, kind))| {
                let id = id.clone();
                let allow = kind.starts_with("allow");
                ui::button(
                    ("servers-permission", i),
                    name.clone(),
                    if allow {
                        ButtonKind::Outline
                    } else {
                        ButtonKind::Ghost
                    },
                    connected,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.open_connected() {
                        cx.emit(ServersEvent::Permission(id.clone()))
                    }
                }))
            });
            return Some(
                band()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(icon("alert-circle", 14.).text_color(theme::attention()))
                            .child(ui::text(
                                Type::Label,
                                theme::text(),
                                format!("Needs permission: {}", p.title),
                            )),
                    )
                    .child(div().flex().flex_wrap().gap(ui::sp(6.)).children(options))
                    .into_any_element(),
            );
        }
        let (_, questions) = open.questions.as_ref()?;
        let at = self.answers.1.len().min(questions.len().saturating_sub(1));
        let q = questions.get(at)?;
        let options = q.options.iter().enumerate().map(|(i, option)| {
            let answer = option.clone();
            ui::button(
                ("servers-option", i),
                option.clone(),
                ButtonKind::Outline,
                connected,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.give_answer(answer.clone(), cx)))
        });
        let count = (questions.len() > 1).then(|| {
            ui::text(
                Type::Caption,
                theme::text_faint(),
                format!("Question {} of {}", at + 1, questions.len()),
            )
        });
        Some(
            band()
                .children(count)
                .child(ui::text(Type::Label, theme::text(), q.question.clone()))
                .child(div().flex().flex_wrap().gap(ui::sp(6.)).children(options))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(ui::field().flex_1().child(self.answer.clone()))
                        .child(
                            ui::button("servers-answer", "Answer", ButtonKind::Outline, connected)
                                .on_click(cx.listener(|this, _, _, cx| this.answer_typed(cx))),
                        )
                        .child(
                            ui::button("servers-skip", "Skip", ButtonKind::Ghost, connected)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.open_connected() {
                                        cx.emit(ServersEvent::SkipQuestion)
                                    }
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_open(&self, open: &Open, cx: &mut Context<Self>) -> AnyElement {
        let connected = self
            .server(&open.server)
            .is_some_and(|s| s.reach == Reach::Connected);
        let (tone, word) = tone(open.status);
        let head = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(10.))
            .h(px(ui::BAR_H))
            .px(ui::sp(12.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Label)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::content())
                            .child(open.title.clone()),
                    )
                    .child(
                        ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            format!("{} · {}", open.agent, open.place),
                        )
                        .truncate(),
                    ),
            )
            .child(ui::status_dot(word, tone))
            .when(open.status == Status::Working, |d| {
                d.child(
                    ui::button("servers-stop", "Stop", ButtonKind::Outline, connected).on_click(
                        cx.listener(|this, _, _, cx| {
                            if this.open_connected() {
                                cx.emit(ServersEvent::Stop)
                            }
                        }),
                    ),
                )
            });
        // Stays for as long as the session is short of what the server has.
        let problem = open.problem.clone().map(|why| {
            let (server, session) = (open.server.clone(), open.session.clone());
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(ui::sp(8.))
                .px(ui::sp(12.))
                .py(ui::sp(6.))
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(icon("alert-circle", 14.).text_color(theme::attention()))
                .child(
                    ui::text(Type::Label, theme::attention().into(), why)
                        .flex_1()
                        .min_w_0(),
                )
                .child(
                    ui::button(
                        "servers-read-again",
                        "Read again",
                        ButtonKind::Outline,
                        connected,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ServersEvent::Open {
                            server: server.clone(),
                            session: session.clone(),
                        })
                    })),
                )
        });
        let body = if open.loading {
            ui::loading("servers-loading", "Reading the session…").into_any_element()
        } else {
            open.view.clone().into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(head)
            .children(problem)
            .child(div().flex().flex_col().flex_1().min_h_0().child(body))
            .children(self.render_asks(open, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(ui::sp(6.))
                    .px(ui::sp(12.))
                    .py(ui::sp(10.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(ui::field().flex_1().child(self.prompt.clone()))
                            .child(
                                ui::button_with(
                                    "servers-send",
                                    Some("arrow-up01"),
                                    "Send",
                                    ButtonKind::Primary,
                                    connected,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                            ),
                    )
                    .when(!connected, |d| {
                        d.child(ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            "The server is not connected. What you type stays here until it is back.",
                        ))
                    }),
            )
            .into_any_element()
    }
}

impl Render for ServersView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self
            .state
            .servers
            .iter()
            .filter(|s| s.reach == Reach::Connected)
            .count();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(8.))
            .h(px(ui::TOOLBAR_H))
            .px(ui::sp(12.))
            .border_b_1()
            .border_color(theme::stroke())
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(icon("computer", 14.).text_color(theme::text_subtle()))
            .child(div().flex_none().child("Servers"))
            .child(div().flex_1())
            .when(!self.state.servers.is_empty(), |d| {
                d.child(div().text_color(theme::text_faint()).child(format!(
                    "{connected} of {} connected",
                    self.state.servers.len()
                )))
                .child(
                    ui::button_with(
                        "servers-add",
                        Some("add01"),
                        "Pair with a server",
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pane = Pane::Add;
                        cx.notify();
                    })),
                )
            });

        let pane = match &self.pane {
            Pane::Add => self.render_add(cx),
            Pane::New(id) => match self.server(id).cloned() {
                Some(server) => self.render_new(&server, cx),
                None => div().into_any_element(),
            },
            Pane::Session => match self.state.open.clone() {
                Some(open) => self.render_open(&open, cx),
                None => ui::empty_state(
                    "computer",
                    "No session open",
                    "Pick a session from the list, or start one with the plus beside a server.",
                    None,
                )
                .into_any_element(),
            },
        };

        let servers: Vec<ServerRow> = self.state.servers.clone();
        let list = (!servers.is_empty()).then(|| {
            div()
                .id("servers-list")
                .flex()
                .flex_col()
                .flex_none()
                .w(px(LIST_W))
                .gap(ui::sp(12.))
                .p(ui::sp(8.))
                .border_r_1()
                .border_color(theme::border_subtle())
                .overflow_y_scroll()
                .children(
                    servers
                        .iter()
                        .enumerate()
                        .map(|(ix, s)| self.render_server(ix, s, cx)),
                )
        });

        // Pairing says what went wrong beside its own field.
        let error = self
            .state
            .error
            .clone()
            .filter(|_| self.pane != Pane::Add)
            .map(|e| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(ui::sp(8.))
                    .px(ui::sp(12.))
                    .py(ui::sp(6.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(icon("alert-circle", 14.).text_color(theme::attention()))
                    .child(ui::text(Type::Label, theme::attention().into(), e))
            });

        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .children(error)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .children(list)
                    .child(div().flex_1().min_w_0().min_h_0().child(pane)),
            )
    }
}
