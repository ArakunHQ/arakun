//! The Servers tab: the machines running `arakun server` this app is paired
//! with, and their sessions. It pairs with the link the server prints and
//! forgets a server. A server's folders and sessions live in the projects,
//! as the app's own (`shell::servers`): a session picked here opens there.

use std::collections::HashSet;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

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
    /// Makes a folder already on the server one sessions can start in.
    AddPlace { server: String, path: String },
    /// Clones a repository on the server, into a folder sessions can start in.
    Clone { server: String, url: String },
    /// Open one of a server's sessions in its project.
    Open { server: String, session: String },
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
    /// Unix seconds of its last activity.
    pub updated: u64,
    /// The folder it runs in, as its place's path. Empty from a server that
    /// does not say: `env`, the folder's name, is all there is then.
    pub place: String,
    pub env: String,
}

impl SessionRow {
    /// Whether the session runs in `place`.
    pub fn in_place(&self, place: &PlaceRow) -> bool {
        if self.place.is_empty() {
            !self.env.is_empty() && self.env == place.project
        } else {
            self.place == place.path
        }
    }
}

/// A folder on the server where a session can start.
#[derive(Clone, PartialEq)]
pub struct PlaceRow {
    pub path: String,
    pub project: String,
    pub branch: String,
    /// The key of the agent sessions start with there.
    pub agent: String,
    /// The repository's main folder there, which its worktrees share.
    pub repo: String,
    /// Its `origin`, by which the same project is found here. Empty when
    /// the server does not say (an older one) or it has none.
    pub origin: String,
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

#[derive(Clone, PartialEq, Default)]
pub struct ServersState {
    pub servers: Vec<ServerRow>,
    /// A pairing is under way.
    pub pairing: bool,
    /// Servers a folder is being added to or cloned on, until they are done.
    pub adding: HashSet<String>,
    /// What went wrong last, until something else happens.
    pub error: Option<String>,
}

/// What the side beside the list shows.
#[derive(Clone, PartialEq)]
enum Pane {
    /// A word on where the sessions are.
    Overview,
    /// The form to pair with a server.
    Add,
    /// The form to clone a repository on this server, or add one of its folders.
    AddPlace(String),
}

/// The list of servers, as wide as a sidebar.
const LIST_W: f32 = 260.;

pub struct ServersView {
    state: ServersState,
    pane: Pane,
    link: Entity<TextInput>,
    /// What to clone on a server, and a folder already there.
    clone_url: Entity<TextInput>,
    folder: Entity<TextInput>,
    /// The server what is in those two was typed for.
    draft_for: Option<String>,
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
        let link = cx.new(|cx| TextInput::new("https://…", cx).mono().redacted());
        let clone_url =
            cx.new(|cx| TextInput::new("https://github.com/owner/repo or owner/repo", cx));
        let folder = cx.new(|cx| TextInput::new("~/code/app", cx).mono());
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
            on_submit(cx, &clone_url, Self::clone_repo),
            on_submit(cx, &folder, Self::add_folder),
        ];
        Self {
            state: ServersState::default(),
            pane: Pane::Add,
            link,
            clone_url,
            folder,
            draft_for: None,
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
            self.pane = Pane::Overview;
        }
        if state.servers.is_empty() {
            self.pane = Pane::Add;
        }
        if let Pane::AddPlace(id) = &self.pane
            && !state.servers.iter().any(|s| s.id == *id)
        {
            self.pane = Pane::Overview;
        }
        self.state = state;
        cx.notify();
    }

    /// The form to clone a repository on `server`, or add one of its folders.
    pub fn add_place(&mut self, server: &str, window: &mut Window, cx: &mut Context<Self>) {
        // What was typed for another server is not meant for this one.
        if self.draft_for.as_deref() != Some(server) {
            self.clone_url.update(cx, |i, cx| i.set_text("", cx));
            self.folder.update(cx, |i, cx| i.set_text("", cx));
            self.draft_for = Some(server.to_string());
        }
        self.pane = Pane::AddPlace(server.to_string());
        window.focus(&self.clone_url.focus_handle(cx), cx);
        cx.notify();
    }

    /// The server added the folder it was asked for, which the shell opens.
    pub fn place_added(&mut self, server: &str, cx: &mut Context<Self>) {
        if self.pane != Pane::AddPlace(server.to_string()) {
            return;
        }
        self.clone_url.update(cx, |i, cx| i.set_text("", cx));
        self.folder.update(cx, |i, cx| i.set_text("", cx));
        self.pane = Pane::Overview;
        cx.notify();
    }

    fn server(&self, id: &str) -> Option<&ServerRow> {
        self.state.servers.iter().find(|s| s.id == id)
    }

    /// The server the add-folder form is for, when it can be asked now.
    fn adding_to(&self) -> Option<String> {
        let Pane::AddPlace(id) = &self.pane else {
            return None;
        };
        let server = self.server(id)?;
        (server.reach == Reach::Connected && !self.state.adding.contains(id)).then(|| id.clone())
    }

    fn clone_repo(&mut self, cx: &mut Context<Self>) {
        let url = self.clone_url.read(cx).text().trim().to_string();
        if let Some(server) = self.adding_to().filter(|_| !url.is_empty()) {
            cx.emit(ServersEvent::Clone { server, url });
        }
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let path = self.folder.read(cx).text().trim().to_string();
        if let Some(server) = self.adding_to().filter(|_| !path.is_empty()) {
            cx.emit(ServersEvent::AddPlace { server, path });
        }
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).text().trim().to_string();
        if !link.is_empty() && !self.state.pairing {
            cx.emit(ServersEvent::Pair(link));
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
        let connected = server.reach == Reach::Connected;
        let forget_id = server.id.clone();
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
                    .child(crate::redact::text(&server.name).into_owned()),
            )
            .when(connected, |d| {
                let id = server.id.clone();
                d.child(
                    ui::icon_button(
                        ("servers-add-place", ix),
                        "folder01",
                        13.,
                        "Clone a repository or add a folder",
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.add_place(&id, window, cx)),
                    ),
                )
            })
            .child(
                ui::icon_button(
                    ("servers-forget", ix),
                    "delete02",
                    13.,
                    "Forget this server",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_forget = Some(forget_id.clone());
                    cx.notify();
                })),
            );
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
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(ServersEvent::Open {
                        server: server_id.clone(),
                        session: session_id.clone(),
                    });
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
                ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    crate::redact::text(&n).into_owned(),
                )
                .px(ui::sp(8.))
                .pb(ui::sp(4.))
            }))
            .children(confirm)
            .children(sessions)
            .when(server.sessions.is_empty() && connected, |d| {
                d.child(
                    ui::text(Type::Caption, theme::text_faint(), "No sessions yet.").px(ui::sp(8.)),
                )
            })
            .into_any_element()
    }

    // ---- a folder to work in ------------------------------------------------

    fn render_add_place(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let connected = server.reach == Reach::Connected;
        let busy = self.state.adding.contains(&server.id);
        let ready = connected && !busy;
        let row = |field: Entity<TextInput>, icon_name: &'static str| {
            ui::field()
                .flex_1()
                .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
                .child(field)
        };
        let part = |label: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(8.))
                .child(Self::label(label))
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(ui::PAGE_PAD))
            .max_w(px(620.))
            .child(ui::page_title(
                format!("A folder on {}", crate::redact::text(&server.name)),
                Some(
                    "Sessions start in the folders a server offers. Clone a repository there, or add one it already has."
                        .into_any_element(),
                ),
            ))
            .child(
                part("Clone a repository, with that machine's git and its logins:").child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(row(self.clone_url.clone(), "git-branch"))
                        .child(
                            ui::button(
                                "servers-clone",
                                if busy { "Working…" } else { "Clone" },
                                ButtonKind::Primary,
                                ready,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.clone_repo(cx))),
                        ),
                ),
            )
            .child(
                part("Or add a folder already on it, as a whole path or from ~/:").child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(row(self.folder.clone(), "folder01"))
                        .child(
                            ui::button("servers-add-folder", "Add", ButtonKind::Outline, ready)
                                .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                        ),
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
                ui::text(Type::Label, theme::attention().into(), crate::redact::text(&e).into_owned())
            }))
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "Through the relay, everything between this app and the server is encrypted end to end. A link that starts with http:// is not encrypted: use one only on a network you trust. Agents there use that machine's logins and run in its folders.",
            ))
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
            Pane::AddPlace(id) => match self.server(id).cloned() {
                Some(server) => self.render_add_place(&server, cx),
                None => div().into_any_element(),
            },
            Pane::Overview => ui::empty_state(
                "computer",
                "Your servers' work is in your projects",
                "Each folder a server offers is an environment in the sidebar: under the project with the same repository here, or under a project of its own. Its sessions open there, as yours do.",
                None,
            )
            .into_any_element(),
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
