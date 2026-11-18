//! The Team tab: one team at a time, as a sheet. A channel shows its chat
//! between the team's channels and people on the left and, when asked, the
//! encryption panel or a device compare on the right. The team's page, its
//! invites and its approvals are pages of their own, as are the steps before
//! a team: starting one, an invite, and waiting for an admin. What it shows
//! comes from the [`Hub`]; this view only draws it and keeps what is typed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, px,
};

use super::Teams;
use super::api::{DeviceRow, InviteRow, Role};
use super::hub::{self, ChannelState, Delivery, DeviceState, Hub, Load, Msg, Notice, TeamState};
use super::socket;
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

// @bot in the chat (its range, its answer, its runs) and the decision log.
#[path = "bot_view.rs"]
mod bot_view;
#[path = "decisions_view.rs"]
mod decisions_view;

pub enum TeamsViewEvent {
    /// Settings → Account, to sign in again.
    OpenSettings,
}

/// What the sheet shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pane {
    /// Nothing picked yet: the first channel once there is one.
    None,
    Channel(String, String),
    /// A team's people (frame "Team created").
    Team(String),
    /// Invite people to a team.
    Invite(String),
    /// People who accepted an invite, waiting for an admin.
    Pending(String),
    NewTeam,
    /// The invites this user got.
    Invites,
    /// This user's own devices.
    Devices,
    /// The team's decision log.
    Decisions(String),
}

/// What the panel at the right of a channel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Side {
    Encryption,
    /// Compare this device with another one, or pick which.
    Compare(Option<String>),
}

/// The left column and the right panel of a channel, as wide as a sidebar.
const COLUMN_W: f32 = 200.;
const PANEL_W: f32 = 320.;
/// A page's column, centered in the sheet.
const PAGE_W: f32 = 520.;
/// Messages from one sender this close together share a header.
const GROUP_SECS: u64 = 5 * 60;
const AVATAR: f32 = 24.;
/// How much of a message a reply quotes.
const SNIPPET: usize = 90;
/// Rows inside a card sit this far from its edge.
const CARD_INSET: f32 = 4.;

/// GitHub pictures on disk, by lower-case login: a copy taken for one render.
struct Faces {
    pictures: HashMap<String, PathBuf>,
    hidden: bool,
}

impl Faces {
    fn of(teams: &Teams) -> Self {
        Faces {
            pictures: teams.pictures(),
            hidden: crate::redact::on(),
        }
    }

    /// The person's picture, round; their initial while there is none, or
    /// while redacted mode hides faces.
    fn avatar(&self, login: &str, size: f32) -> gpui::Div {
        let picture = self
            .pictures
            .get(&login.to_ascii_lowercase())
            .filter(|_| !self.hidden);
        ui::avatar_image(login, picture.map(|p| p.as_path()), size)
    }
}

/// A row of a channel's list, in order.
enum Line {
    Day(String),
    /// An admin let this device in, during this run.
    Joined,
    Notice(Notice),
    Message(usize),
    /// "Decided.", under the @bot answer at this index.
    Decided(usize),
    /// A run of @bot from this device, by its place in the channel's runs.
    Run(usize),
}

/// How a message stands out in the list.
#[derive(Clone, Copy, Default)]
struct Mark {
    /// In the range @bot would read, while @bot is being asked.
    in_range: bool,
    /// Its number in the range a run of @bot reads.
    number: Option<usize>,
}

pub struct TeamsView {
    teams: Entity<Teams>,
    hub: Entity<Hub>,
    pane: Pane,
    side: Option<Side>,
    /// The teams menu under the team's name is open.
    team_menu: bool,
    /// The team picked last, for the bar over pages of no team (your devices).
    last_team: Option<String>,
    /// The field for a new channel shows in the channel list.
    adding_channel: bool,
    /// Teams there were when "Create team" was pressed: the new one is the
    /// one not among them, and its page opens once it arrives.
    creating_from: Option<HashSet<String>>,
    team_name: Entity<TextInput>,
    invite: Entity<TextInput>,
    channel_name: Entity<TextInput>,
    composer: Entity<TextInput>,
    device_code: Entity<TextInput>,
    /// What was typed in other channels, by channel key.
    drafts: HashMap<String, String>,
    /// The message the composer answers.
    reply_to: Option<String>,
    scroll: ScrollHandle,
    /// Rows in the open channel at the last render (twice the messages,
    /// plus @bot's runs): new ones keep the list at its bottom.
    seen_count: usize,
    /// A message just jumped to, lit for a moment.
    flash: Option<String>,
    _flash: Option<Task<()>>,
    /// "Remove" or "Leave" was pressed once on this member.
    confirm_remove: Option<(String, u64)>,
    /// "They don't" was pressed for this device in the compare panel.
    mismatch: Option<String>,
    copied: bool,
    /// Decided @bot answers opened again, by message ID: they fold once
    /// this user accepted them.
    unfolded: HashSet<String>,
    /// A message to scroll to once its channel shows it.
    pending_jump: Option<String>,
    /// The decision open in the Decisions panel, by its bot message ID.
    decision: Option<String>,
    /// Decisions shows only the ones this user accepted or proposed.
    decisions_mine: bool,
    decision_query: Entity<TextInput>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TeamsViewEvent> for TeamsView {}

impl TeamsView {
    pub fn new(teams: Entity<Teams>, cx: &mut Context<Self>) -> Self {
        let hub = cx.new(|cx| Hub::new(teams.clone(), cx));
        let team_name = cx.new(|cx| TextInput::new("Arakun", cx));
        let invite = cx.new(|cx| TextInput::new("GitHub login", cx).mono());
        let channel_name = cx.new(|cx| TextInput::new("new-channel", cx));
        let composer = cx.new(|cx| TextInput::new("Message", cx));
        let device_code = cx.new(|cx| TextInput::new("arakun-device.…", cx).mono());
        let decision_query =
            cx.new(|cx| TextInput::new("Who decided what? Search a file, a word, a person", cx));
        let on = |cx: &mut Context<Self>,
                  input: &Entity<TextInput>,
                  act: fn(&mut Self, InputEvent, &mut Context<Self>)| {
            cx.subscribe(input, move |this, _, ev: &InputEvent, cx| {
                act(this, *ev, cx)
            })
        };
        let subs = vec![
            cx.observe(&teams, |_, _, cx| cx.notify()),
            cx.observe(&hub, |this, _, cx| this.hub_changed(cx)),
            on(cx, &team_name, |this, ev, cx| {
                if ev == InputEvent::Submit {
                    this.create_team(cx)
                }
            }),
            on(cx, &invite, |this, ev, cx| match ev {
                InputEvent::Submit => this.send_invite(cx),
                _ => cx.notify(),
            }),
            on(cx, &channel_name, |this, ev, cx| match ev {
                InputEvent::Submit => this.create_channel(cx),
                InputEvent::Cancel => {
                    this.adding_channel = false;
                    cx.notify();
                }
                _ => {}
            }),
            on(cx, &composer, |this, ev, cx| match ev {
                InputEvent::Submit => this.send(cx),
                InputEvent::Cancel => {
                    this.reply_to = None;
                    cx.notify();
                }
                _ => cx.notify(),
            }),
            on(cx, &device_code, |_, _, cx| cx.notify()),
            on(cx, &decision_query, |_, _, cx| cx.notify()),
        ];
        Self {
            teams,
            hub,
            pane: Pane::None,
            side: None,
            team_menu: false,
            last_team: None,
            adding_channel: false,
            creating_from: None,
            team_name,
            invite,
            channel_name,
            composer,
            device_code,
            drafts: HashMap::new(),
            reply_to: None,
            scroll: ScrollHandle::new(),
            seen_count: 0,
            flash: None,
            _flash: None,
            confirm_remove: None,
            mismatch: None,
            copied: false,
            unfolded: HashSet::new(),
            pending_jump: None,
            decision: None,
            decisions_mine: false,
            decision_query,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.hub.update(cx, |h, cx| h.refresh_unread(true, cx));
    }

    /// Tells the hub which channel shows, so what arrives there is read.
    fn sync_viewing(&self, cx: &mut Context<Self>) {
        let key = match &self.pane {
            Pane::Channel(t, c) => Some(hub::channel_key(t, c)),
            _ => None,
        };
        self.hub.update(cx, |h, cx| h.set_viewing(key, cx));
    }

    /// The project @bot reads: the environment the window shows.
    pub fn set_project(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        self.hub.update(cx, |h, _| h.set_bot_project(Some(dir)));
    }

    /// Opens the first channel once there is one and nothing else was picked,
    /// opens a team just created, keeps the pane on things that still exist,
    /// and asks for the pictures of everyone the tab can show.
    fn hub_changed(&mut self, cx: &mut Context<Self>) {
        let hub = self.hub.read(cx);
        let gone = match &self.pane {
            Pane::Channel(t, c) => !hub
                .team
                .get(t)
                .is_some_and(|s| s.channels.iter().any(|ch| ch.id == *c)),
            Pane::Team(t) | Pane::Invite(t) | Pane::Pending(t) | Pane::Decisions(t) => {
                !hub.teams.iter().any(|r| r.team_id == *t)
            }
            _ => false,
        };
        // A team still loading is not gone yet.
        let loading = match &self.pane {
            Pane::Channel(t, _) => {
                hub.teams.iter().any(|r| r.team_id == *t) && !hub.team.contains_key(t)
            }
            _ => false,
        };
        let created = self.creating_from.as_ref().and_then(|before| {
            hub.teams
                .iter()
                .find(|r| !before.contains(&r.team_id) && hub.team.contains_key(&r.team_id))
                .map(|r| r.team_id.clone())
        });
        let mut people: Vec<String> = Vec::new();
        people.extend(hub.me().map(|m| m.login.clone()));
        for t in hub.team.values() {
            people.extend(t.detail.members.iter().map(|m| m.login.clone()));
            people.extend(t.pending.iter().map(|p| p.login.clone()));
        }
        people.extend(
            hub.invites
                .iter()
                .filter_map(|i| i.invited_by_login.clone()),
        );
        people.extend(hub.inviters.values().cloned());
        people.extend(hub.sent.iter().map(|(_, l, _)| l.clone()));
        if let Some(team) = created {
            self.creating_from = None;
            self.pane = Pane::Team(team);
        } else if gone && !loading {
            self.pane = Pane::None;
            self.side = None;
        }
        if self.pane == Pane::None {
            let hub = self.hub.read(cx);
            let first = hub.teams.iter().find_map(|r| {
                let t = hub.team.get(&r.team_id)?;
                let c = t.channels.first()?;
                Some((r.team_id.clone(), c.id.clone()))
            });
            if let Some((t, c)) = first {
                self.open_channel(t, c, cx);
            }
        }
        self.teams.update(cx, |t, cx| t.want_pictures(people, cx));
        self.sync_viewing(cx);
        cx.notify();
    }

    fn open_channel(&mut self, team: String, channel: String, cx: &mut Context<Self>) {
        let key = hub::channel_key(&team, &channel);
        if let Pane::Channel(t, c) = &self.pane {
            let was = hub::channel_key(t, c);
            if was == key {
                return;
            }
            let typed = self.composer.read(cx).text().to_string();
            if !typed.trim().is_empty() {
                self.drafts.insert(was, typed);
            }
        }
        let name = self
            .hub
            .read(cx)
            .team
            .get(&team)
            .and_then(|t| t.channels.iter().find(|c| c.id == channel))
            .and_then(|c| c.name.clone())
            .unwrap_or_else(|| "this channel".into());
        let draft = self.drafts.remove(&key).unwrap_or_default();
        self.composer.update(cx, |i, cx| {
            i.set_text(draft, cx);
            i.set_placeholder(format!("Message {name}"), cx);
        });
        self.reply_to = None;
        self.seen_count = 0;
        self.flash = None;
        self.hub
            .update(cx, |h, cx| h.open_channel(&team, &channel, cx));
        self.pane = Pane::Channel(team, channel);
        self.sync_viewing(cx);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Shows a pane, with the cursor in its field when it has one.
    fn show(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        self.team_menu = false;
        if let Some(t) = self.selected_team() {
            self.last_team = Some(t.to_string());
        }
        let field = match &pane {
            Pane::NewTeam => Some(self.team_name.clone()),
            Pane::Devices => Some(self.device_code.clone()),
            Pane::Invite(_) => Some(self.invite.clone()),
            Pane::Channel(..) => Some(self.composer.clone()),
            Pane::Decisions(_) => Some(self.decision_query.clone()),
            _ => None,
        };
        if let Pane::Team(t) | Pane::Pending(t) = &pane {
            let t = t.clone();
            self.hub.update(cx, |h, cx| h.load_team(&t, cx));
        }
        if let Pane::Decisions(t) = &pane {
            self.open_every_channel(t, cx);
        }
        if let Pane::Channel(t, c) = pane {
            self.open_channel(t, c, cx);
        } else {
            self.pane = pane;
            self.sync_viewing(cx);
        }
        if let Some(field) = field {
            let handle = field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// The team the sheet is about.
    fn selected_team(&self) -> Option<&str> {
        match &self.pane {
            Pane::Channel(t, _)
            | Pane::Team(t)
            | Pane::Invite(t)
            | Pane::Pending(t)
            | Pane::Decisions(t) => Some(t),
            _ => None,
        }
    }

    /// A team's first channel, to open from its pages.
    fn first_channel(&self, team_id: &str, cx: &Context<Self>) -> Option<(String, String)> {
        let c = self.hub.read(cx).team.get(team_id)?.channels.first()?;
        Some((team_id.to_string(), c.id.clone()))
    }

    fn create_team(&mut self, cx: &mut Context<Self>) {
        let name = self.team_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.team_name.update(cx, |i, cx| i.set_text("", cx));
        let before = self
            .hub
            .read(cx)
            .teams
            .iter()
            .map(|r| r.team_id.clone())
            .collect();
        self.creating_from = Some(before);
        self.hub.update(cx, |h, cx| h.create_team(name, cx));
        cx.notify();
    }

    fn send_invite(&mut self, cx: &mut Context<Self>) {
        let Some(team) = self.selected_team().map(str::to_string) else {
            return;
        };
        let login = self
            .invite
            .read(cx)
            .text()
            .trim()
            .trim_start_matches('@')
            .to_string();
        if login.is_empty() {
            return;
        }
        self.invite.update(cx, |i, cx| i.set_text("", cx));
        self.hub.update(cx, |h, cx| h.invite(&team, login, cx));
    }

    fn create_channel(&mut self, cx: &mut Context<Self>) {
        let Some(team) = self.selected_team().map(str::to_string) else {
            return;
        };
        let name = self
            .channel_name
            .read(cx)
            .text()
            .trim()
            .trim_start_matches('#')
            .to_string();
        if name.is_empty() {
            return;
        }
        self.channel_name.update(cx, |i, cx| i.set_text("", cx));
        self.adding_channel = false;
        self.hub
            .update(cx, |h, cx| h.create_channel(&team, name, cx));
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let Pane::Channel(team, channel) = self.pane.clone() else {
            return;
        };
        let text = self.composer.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let reply_to = self.reply_to.take();
        let sent = self.hub.update(cx, |h, cx| {
            h.send(&team, &channel, text, reply_to.clone(), cx)
        });
        if sent {
            self.composer.update(cx, |i, cx| i.set_text("", cx));
            self.scroll.scroll_to_bottom();
        } else {
            self.reply_to = reply_to;
        }
        cx.notify();
    }

    /// Scrolls to a message and lights it for a moment.
    fn jump_to(&mut self, index: usize, id: String, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(index);
        self.flash = Some(id);
        self._flash = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1600))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.flash = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_side(&mut self, side: Side, cx: &mut Context<Self>) {
        self.side = if self.side.as_ref() == Some(&side) {
            None
        } else {
            Some(side)
        };
        self.mismatch = None;
        cx.notify();
    }

    // ---- pieces ------------------------------------------------------------------

    fn banner(icon_name: &str, tone: Tone, text: impl Into<SharedString>) -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(8.))
            .px(ui::sp(16.))
            .py(ui::sp(6.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(icon(icon_name, 14.).text_color(tone.color()))
            .child(ui::text(Type::Label, tone.color(), text).flex_1().min_w_0())
    }

    /// A page's column: centered, as wide as a form.
    fn page() -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(20.))
            .w(px(PAGE_W))
            .max_w_full()
            .mx_auto()
            .py(ui::sp(32.))
    }

    fn scrolling(id: &'static str, page: gpui::Div) -> AnyElement {
        div()
            .id(id)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px(ui::sp(16.))
            .overflow_y_scroll()
            .child(page)
            .into_any_element()
    }

    /// A raised card on a page; its rows sit `CARD_INSET` in.
    fn card() -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::fill_raised())
    }

    /// A card's rows, `CARD_INSET` from its edge.
    fn rows() -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(2.))
            .p(ui::sp(CARD_INSET))
    }

    /// The square a row's picture or icon sits on.
    fn tile(child: impl IntoElement) -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(ui::BUTTON_H))
            .rounded(ui::rad(ui::RADIUS_SM))
            .bg(theme::fill_selected())
            .child(child)
    }

    /// A person (or an action) in a card: a tile, a name and a note, and
    /// what goes at the end.
    fn person_row(
        lead: impl IntoElement,
        title: impl Into<SharedString>,
        note: impl Into<SharedString>,
        selected: bool,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .px(ui::sp(10.))
            .py(ui::sp(8.))
            .rounded(ui::nested(ui::RADIUS_LG, CARD_INSET))
            .when(selected, |d| d.bg(theme::fill_hover()))
            .child(Self::tile(lead))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Body)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(title.into()),
                    )
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(note.into()),
                    ),
            )
    }

    /// An icon and a sentence, under a form or in a card.
    fn note(icon_name: &str, text: impl Into<SharedString>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::text_subtle())
            .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
            .child(div().flex_1().min_w_0().child(text.into()))
    }

    /// A label and its value, a line above: the panel's facts.
    fn fact(label: impl Into<SharedString>, value: impl IntoElement) -> gpui::Div {
        div()
            .flex()
            .justify_between()
            .items_center()
            .gap(ui::sp(16.))
            .py(ui::sp(8.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .ty(Type::Body)
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_subtle())
                    .child(label.into()),
            )
            .child(div().text_color(theme::text()).text_right().child(value))
    }

    fn footnote(text: impl Into<SharedString>) -> gpui::Div {
        ui::text(Type::Label, theme::text_subtle(), text).font_weight(FontWeight::NORMAL)
    }

    /// A rail-style title over a group in the channel column.
    fn column_title(title: &'static str) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(4.))
            .h(px(ui::BUTTON_H))
            .px(ui::sp(8.))
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(div().flex_1().min_w_0().truncate().child(title))
    }

    // ---- the header ------------------------------------------------------------------

    /// The sheet's bar: the team, the channel, "Encrypted", the people and
    /// Decisions.
    fn render_header(
        &self,
        team_id: &str,
        channel: Option<String>,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hub = self.hub.read(cx);
        let row = hub.teams.iter().find(|r| r.team_id == team_id);
        let t = hub.team.get(team_id);
        let name = t
            .map(|t| t.detail.name.clone())
            .or_else(|| row.map(|r| r.name.clone()))
            .unwrap_or_default();
        let admin = row.is_some_and(|r| r.role.is_admin());
        let waiting = t.map_or(0, |t| t.pending.len());
        let members: Vec<String> = t
            .map(|t| t.detail.members.iter().map(|m| m.login.clone()).collect())
            .unwrap_or_default();
        let in_channel = channel.is_some();
        let encryption_open = self.side == Some(Side::Encryption);
        let tid = team_id.to_string();
        let tid2 = team_id.to_string();
        let tid3 = team_id.to_string();
        let tid4 = team_id.to_string();
        let on_decisions = matches!(&self.pane, Pane::Decisions(_));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(8.))
            .h(px(ui::BAR_H))
            .pl(ui::sp(16.))
            .pr(ui::sp(12.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                // A chip that sits flush with the bar's padding until hovered.
                div()
                    .id("teams-team-name")
                    .flex()
                    .items_center()
                    .min_w_0()
                    .h(px(ui::CONTROL_H))
                    .px(ui::sp(6.))
                    .ml(-ui::sp(6.))
                    .mr(-ui::sp(4.))
                    .rounded(ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .when(self.team_menu, |d| d.bg(theme::fill_selected()))
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Body)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::content())
                            .child(name),
                    )
                    .tooltip(ui::tip("Teams, devices, a new team"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.team_menu = !this.team_menu;
                        cx.notify();
                    })),
            )
            .children(channel.map(|c| {
                div()
                    .truncate()
                    .ty(Type::Body)
                    .text_color(theme::text_subtle())
                    .child(c)
            }))
            .child(
                div()
                    .id("teams-encrypted")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(ui::sp(6.))
                    .h(px(ui::CONTROL_H))
                    .px(ui::sp(8.))
                    .rounded(ui::rad(ui::RADIUS))
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_subtle())
                    .when(encryption_open, |d| d.bg(theme::fill_selected()))
                    .when(in_channel, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.toggle_side(Side::Encryption, cx)
                                }),
                            )
                    })
                    .child(icon("square-lock02", 12.).text_color(theme::text_subtle()))
                    .child("Encrypted"),
            )
            .child(div().flex_1())
            .when(admin && waiting > 0, |d| {
                d.child(
                    ui::button_lead(
                        "teams-waiting",
                        Some(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(theme::needs_you())
                                .into_any_element(),
                        ),
                        format!("{waiting} waiting"),
                        ButtonKind::Ghost,
                        true,
                    )
                    .tooltip(ui::tip("People waiting for you to approve them"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(Pane::Pending(tid.clone()), window, cx)
                    })),
                )
            })
            .when(!members.is_empty() && in_channel, |d| {
                d.child(
                    div()
                        .id("teams-people")
                        .flex()
                        .items_center()
                        .gap(ui::sp(4.))
                        .px(ui::sp(4.))
                        .h(px(ui::CONTROL_H))
                        .rounded(ui::rad(ui::RADIUS))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::fill_hover()))
                        .tooltip(ui::tip("The team's people"))
                        .children(members.iter().take(6).map(|l| faces.avatar(l, 20.)))
                        .when(members.len() > 6, |d| {
                            d.child(ui::text(
                                Type::Caption,
                                theme::text_subtle(),
                                format!("+{}", members.len() - 6),
                            ))
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show(Pane::Team(tid2.clone()), window, cx)
                        })),
                )
            })
            .when(!in_channel, |d| {
                d.when_some(self.first_channel(&tid3, cx), |d, (t, c)| {
                    d.child(
                        ui::button_with(
                            "teams-to-chat",
                            Some("comment01"),
                            "Chat",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.show(Pane::Channel(t.clone(), c.clone()), window, cx)
                            },
                        )),
                    )
                })
            })
            .child(
                ui::button("teams-decisions", "Decisions", ButtonKind::Ghost, true)
                    .when(on_decisions, |d| d.bg(theme::fill_selected()))
                    .tooltip(ui::tip("What the team decided with @bot"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(Pane::Decisions(tid4.clone()), window, cx)
                    })),
            )
            .into_any_element()
    }

    /// The menu under the team's name: every team, a new one, your devices.
    fn render_team_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let current = self.selected_team().map(str::to_string);
        // Each team's ID, name and first channel.
        type Entry = (String, String, Option<(String, String)>);
        let teams: Vec<Entry> = hub
            .teams
            .iter()
            .map(|r| {
                let first = hub
                    .team
                    .get(&r.team_id)
                    .and_then(|t| t.channels.first())
                    .map(|c| (r.team_id.clone(), c.id.clone()));
                (r.team_id.clone(), r.name.clone(), first)
            })
            .collect();
        let has_invites = !hub.invites.is_empty();
        let mut panel = ui::menu_panel("teams-menu", 240.).p(ui::sp(5.));
        for (i, (id, name, first)) in teams.into_iter().enumerate() {
            let on = current.as_deref() == Some(id.as_str());
            panel = panel.child(
                ui::menu_item(("teams-menu-team", i), "ungroup-items", name, false, true)
                    .when(on, |d| d.bg(theme::selection()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let pane = match &first {
                            Some((t, c)) => Pane::Channel(t.clone(), c.clone()),
                            None => Pane::Team(id.clone()),
                        };
                        this.show(pane, window, cx)
                    })),
            );
        }
        panel
            .child(ui::menu_divider())
            .when(has_invites, |d| {
                d.child(
                    ui::menu_item("teams-menu-invites", "mail01", "Invites", false, true).on_click(
                        cx.listener(|this, _, window, cx| this.show(Pane::Invites, window, cx)),
                    ),
                )
            })
            .child(
                ui::menu_item("teams-menu-new", "add01", "Start a team", false, true).on_click(
                    cx.listener(|this, _, window, cx| this.show(Pane::NewTeam, window, cx)),
                ),
            )
            .child(
                ui::menu_item(
                    "teams-menu-devices",
                    "computer",
                    "Your devices",
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.hub.update(cx, |h, cx| h.load_my_devices(cx));
                    this.show(Pane::Devices, window, cx)
                })),
            )
            .into_any_element()
    }

    // ---- pages before a team -------------------------------------------------------

    /// "Start a team".
    fn render_new_team(&self, cx: &mut Context<Self>) -> AnyElement {
        let creating =
            self.hub.read(cx).busy.contains("create-team") || self.creating_from.is_some();
        Self::scrolling(
            "teams-new-page",
            Self::page()
                .child(ui::page_title(
                    "Start a team",
                    Some(
                        "A place to talk ideas through, with @bot to check them. You will be its owner."
                            .into_any_element(),
                    ),
                ))
                .child(
                    Self::card().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(12.))
                            .p(ui::sp(16.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(ui::sp(6.))
                                    .child(
                                        ui::text(Type::Body, theme::text(), "Team name")
                                            .font_weight(FontWeight::MEDIUM),
                                    )
                                    .child(
                                        ui::field()
                                            .child(
                                                icon("ungroup-items", 14.)
                                                    .text_color(theme::text_subtle()),
                                            )
                                            .child(div().flex_1().child(self.team_name.clone())),
                                    ),
                            )
                            .child(Self::note(
                                "square-lock02",
                                "This device makes the team key. The server never gets it.",
                            ))
                            .child(
                                div().flex().justify_end().child(
                                    ui::button(
                                        "teams-create",
                                        if creating { "Creating…" } else { "Create team" },
                                        ButtonKind::Primary,
                                        !creating,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.create_team(cx))),
                                ),
                            ),
                    ),
                ),
        )
    }

    /// The invites this user got, as the invitee sees them.
    fn render_invites(&self, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let invites: Vec<(InviteRow, bool)> = hub
            .invites
            .iter()
            .map(|i| {
                let busy = hub.busy.contains(&format!("accept:{}", i.invite_id));
                (i.clone(), busy)
            })
            .collect();
        let me = hub
            .me()
            .map(|m| crate::redact::value(&m.login))
            .unwrap_or_default();
        let mut page = Self::page().child(ui::page_title(
            if invites.len() > 1 {
                "Invites"
            } else {
                "An invite"
            },
            Some(format!("You are signed in as {me}.").into_any_element()),
        ));
        if invites.is_empty() {
            page = page.child(Self::footnote("No invites are waiting."));
        }
        for (ix, (inv, busy)) in invites.into_iter().enumerate() {
            let by = inv.invited_by_login.clone();
            let title = match &by {
                Some(by) => format!("{by} invited you to {}", inv.team_name),
                None => format!("You are invited to {}", inv.team_name),
            };
            let accept = inv.clone();
            page = page.child(
                Self::card().child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(ui::sp(16.))
                        .p(ui::sp(16.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(ui::sp(12.))
                                .child(faces.avatar(by.as_deref().unwrap_or(&inv.team_name), 32.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(ui::sp(3.))
                                        .min_w_0()
                                        .child(
                                            div()
                                                .truncate()
                                                .ty(Type::Heading)
                                                .text_color(theme::text())
                                                .child(title),
                                        )
                                        .child(
                                            ui::text(
                                                Type::Label,
                                                theme::text_subtle(),
                                                format!(
                                                    "Invited as {me} · {}",
                                                    ago(inv.created_at)
                                                ),
                                            )
                                            .font_weight(FontWeight::NORMAL),
                                        ),
                                ),
                        )
                        .child(Self::note(
                            "square-lock02",
                            "After you accept, an admin approves this device. Then it can read the team.",
                        ))
                        .child(
                            div().flex().justify_end().gap(ui::sp(8.)).child(
                                ui::button(
                                    ("teams-accept", ix),
                                    if busy { "Joining…" } else { "Accept" },
                                    ButtonKind::Primary,
                                    !busy,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.hub.update(cx, |h, cx| h.accept(&accept, cx));
                                })),
                            ),
                        ),
                ),
            );
        }
        Self::scrolling("teams-invites-page", page)
    }

    /// "Waiting for an admin to approve this device": after accepting an
    /// invite, or a device of a member that holds no team key yet.
    fn render_waiting(
        &self,
        team: &str,
        subtitle: &'static str,
        admins: Vec<String>,
        faces: &Faces,
    ) -> AnyElement {
        let device = crate::redact::value(&hub::device_name());
        let who: AnyElement = if admins.is_empty() {
            "An admin of the team".into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(ui::sp(4.))
                .children(admins.iter().take(6).map(|l| faces.avatar(l, 20.)))
                .child(
                    div().ml(ui::sp(4.)).child(
                        admins
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                )
                .into_any_element()
        };
        Self::scrolling(
            "teams-waiting-page",
            Self::page()
                .child(ui::page_title(
                    team.to_string(),
                    Some(subtitle.into_any_element()),
                ))
                .child(
                    Self::card().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(16.))
                            .p(ui::sp(16.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(ui::sp(12.))
                                    .child(Self::tile(ui::spinner(
                                        "teams-waiting-spin",
                                        16.,
                                        theme::accent(),
                                    )))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(ui::sp(3.))
                                            .min_w_0()
                                            .child(ui::text(
                                                Type::Heading,
                                                theme::text(),
                                                "Waiting for an admin to approve this device",
                                            ))
                                            .child(
                                                ui::text(
                                                    Type::Label,
                                                    theme::text_subtle(),
                                                    format!("{device} · asked to join"),
                                                )
                                                .font_weight(FontWeight::NORMAL),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(Self::fact("Who can approve", who))
                                    .child(Self::fact(
                                        "What you get",
                                        "The team key, sent to this device only",
                                    ))
                                    .child(Self::fact("Until then", "You can't read or write here"))
                                    .child(div().border_t_1().border_color(theme::border_subtle())),
                            )
                            .child(Self::note(
                                "square-lock02",
                                "An admin's device holds the key. The server does not.",
                            )),
                    ),
                ),
        )
    }

    // ---- a team's pages ------------------------------------------------------------

    /// The team's people, with "Invite people", and a way to the approvals.
    fn render_team(&self, team_id: &str, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return ui::loading("teams-team-loading", "Reading the team…").into_any_element();
        };
        let me = hub.me().map_or(0, |m| m.github_id);
        let admin = t.role.is_admin();
        let role = t.role;
        let members = t.detail.members.clone();
        let devices: HashMap<u64, Vec<DeviceRow>> = t.devices.clone();
        let pending = t.pending.len();
        let problem = t.problem.clone();
        let name = t.detail.name.clone();
        let first = t.channels.first().map(|c| (c.id.clone(), c.name.clone()));
        let device_count: usize = devices.values().map(Vec::len).sum();
        let owners = members.iter().filter(|m| m.role == Role::Owner).count();
        let tid = team_id.to_string();

        let subtitle = if members.len() == 1 && device_count <= 1 {
            "Your team is ready. It has one person and one device.".to_string()
        } else {
            format!(
                "{} · {}. You are {}.",
                plural(members.len(), "person", "people"),
                plural(device_count, "device", "devices"),
                match role {
                    Role::Owner => "its owner",
                    Role::Admin => "an admin",
                    Role::Member => "a member",
                }
            )
        };
        let mut rows = Self::rows();
        for (i, m) in members.iter().enumerate() {
            let theirs = devices.get(&m.github_id).cloned().unwrap_or_default();
            let held = match theirs.as_slice() {
                [one] => format!("{} holds the key", crate::redact::text(&one.name)),
                more => plural(more.len(), "device", "devices"),
            };
            let role_word = m.role.word();
            let self_row = m.github_id == me;
            let confirming = self.confirm_remove.as_ref() == Some(&(tid.clone(), m.github_id));
            let last_owner = m.role == Role::Owner && owners < 2;
            let can_remove = !last_owner
                && (self_row || (admin && m.role != Role::Owner) || role == Role::Owner);
            let gid = m.github_id;
            let (t1, t2) = (tid.clone(), tid.clone());
            rows = rows.child(
                Self::person_row(
                    faces.avatar(&m.login, AVATAR),
                    m.login.clone(),
                    format!("{role_word} · {held}"),
                    false,
                )
                .when(self_row, |d| d.child(ui::badge("You", Tone::Neutral)))
                .when(can_remove && !confirming, |d| {
                    d.child(
                        ui::button(
                            ("teams-remove", i),
                            if self_row { "Leave" } else { "Remove" },
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = Some((t1.clone(), gid));
                            cx.notify();
                        })),
                    )
                })
                .when(confirming, |d| {
                    d.child(
                        ui::button(("teams-remove-no", i), "Keep", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_remove = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::button(
                            ("teams-remove-yes", i),
                            if self_row { "Leave the team" } else { "Remove" },
                            ButtonKind::Danger,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = None;
                            this.hub.update(cx, |h, cx| h.remove(&t2, gid, cx));
                        })),
                    )
                }),
            );
        }
        if admin {
            let t3 = tid.clone();
            rows = rows.child(
                Self::person_row(
                    icon("add01", 16.).text_color(theme::text_subtle()),
                    "Invite people",
                    "By their GitHub login",
                    false,
                )
                .child(
                    ui::button("teams-to-invite", "Invite", ButtonKind::Outline, true).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.show(Pane::Invite(t3.clone()), window, cx)
                        }),
                    ),
                ),
            );
        }
        let mut page = Self::page()
            .child(ui::page_title(name, Some(subtitle.into_any_element())))
            .child(Self::card().child(rows));
        if admin && pending > 0 {
            let t4 = tid.clone();
            page = page.child(
                Self::card().child(
                    Self::rows().child(
                        Self::person_row(
                            icon("clock01", 16.).text_color(theme::needs_you()),
                            format!("{} waiting for you", plural(pending, "person", "people")),
                            "They accepted. Approve each of their devices.",
                            false,
                        )
                        .child(
                            ui::button("teams-to-pending", "Review", ButtonKind::Outline, true)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.show(Pane::Pending(t4.clone()), window, cx)
                                })),
                        ),
                    ),
                ),
            );
        }
        if let Some(p) = problem {
            page = page.child(ui::text(Type::Label, theme::attention().into(), p));
        }
        if let Some((cid, cname)) = first {
            let t5 = tid.clone();
            let cname = cname.unwrap_or_else(|| "general".into());
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(12.))
                    .child(
                        Self::footnote(format!(
                            "A {cname} channel is waiting. Its name is encrypted like everything else."
                        ))
                        .flex_1(),
                    )
                    .child(
                        ui::button_with(
                            "teams-open-first",
                            Some("comment01"),
                            format!("Open {cname}"),
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show(Pane::Channel(t5.clone(), cid.clone()), window, cx)
                        })),
                    ),
            );
        }
        Self::scrolling("teams-team-page", page)
    }

    /// Invite people by GitHub login, and the invites sent from here.
    fn render_invite(&self, team_id: &str, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let inviting = hub.busy.contains(&format!("invite:{team_id}"));
        let t = hub.team.get(team_id);
        let pending: HashSet<String> = t
            .map(|t| {
                t.pending
                    .iter()
                    .map(|p| p.login.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let members: HashSet<String> = t
            .map(|t| {
                t.detail
                    .members
                    .iter()
                    .map(|m| m.login.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let sent: Vec<(String, u64)> = hub
            .sent
            .iter()
            .rev()
            .filter(|(t, l, _)| t == team_id && !members.contains(&l.to_ascii_lowercase()))
            .map(|(_, l, at)| (l.clone(), *at))
            .collect();
        let typed = self
            .invite
            .read(cx)
            .text()
            .trim()
            .trim_start_matches('@')
            .to_string();
        let mut card = Self::card().child(
            div()
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .p(ui::sp(12.))
                .child(
                    ui::field()
                        .flex_1()
                        .child(icon("github", 14.).text_color(theme::text_subtle()))
                        .child(div().flex_1().child(self.invite.clone())),
                )
                .child(
                    ui::button(
                        "teams-invite",
                        if inviting { "Inviting…" } else { "Invite" },
                        ButtonKind::Primary,
                        !inviting && !typed.is_empty(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.send_invite(cx))),
                ),
        );
        if !typed.is_empty() {
            card = card.child(
                div().px(ui::sp(CARD_INSET)).pb(ui::sp(CARD_INSET)).child(
                    Self::person_row(
                        faces.avatar(&typed, AVATAR),
                        typed.clone(),
                        "A GitHub login",
                        true,
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(ui::sp(6.))
                            .h(px(22.))
                            .px(ui::sp(8.))
                            .rounded(ui::rad(ui::RADIUS_SM))
                            .border_1()
                            .border_color(theme::border())
                            .ty(Type::Caption)
                            .text_color(theme::text_muted())
                            .child(div().size(px(5.)).rounded_full().bg(theme::text_muted()))
                            .child("Enter to invite"),
                    ),
                ),
            );
        }
        let mut page = Self::page()
            .child(ui::page_title(
                "Invite people",
                Some(
                    "By GitHub login. They accept, then an admin approves each of their devices."
                        .into_any_element(),
                ),
            ))
            .child(card);
        if !sent.is_empty() {
            let mut rows = Self::rows();
            for (login, at) in sent {
                let note = if pending.contains(&login.to_ascii_lowercase()) {
                    format!("Invited {} · accepted, waiting for approval", ago(at))
                } else {
                    format!("Invited {} · has not answered", ago(at))
                };
                rows = rows.child(Self::person_row(
                    faces.avatar(&login, AVATAR),
                    login,
                    note,
                    false,
                ));
            }
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(10.))
                    .child(ui::text(Type::Overline, theme::text_subtle(), "INVITED").px(ui::sp(4.)))
                    .child(Self::card().child(rows)),
            );
        }
        Self::scrolling(
            "teams-invite-page",
            page.child(Self::footnote(
                "An invite lasts 7 days. It shows in their Team tab once they sign in.",
            )),
        )
    }

    /// People who accepted, each device with its code: approve or reject.
    fn render_pending(&self, team_id: &str, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return ui::loading("teams-pending-loading", "Reading the team…").into_any_element();
        };
        let keyless = t.keyless;
        let pending = t.pending.clone();
        let approving: Vec<bool> = pending
            .iter()
            .map(|p| {
                hub.busy
                    .contains(&format!("approve:{team_id}:{}", p.github_id))
            })
            .collect();
        let tid = team_id.to_string();
        let mut rows = Self::rows();
        for (i, p) in pending.iter().enumerate() {
            let (t1, t2, gid) = (tid.clone(), tid.clone(), p.github_id);
            let busy = approving[i];
            rows = rows
                .child(
                    Self::person_row(
                        faces.avatar(&p.login, AVATAR),
                        p.login.clone(),
                        format!(
                            "Accepted {} · {}",
                            ago(p.requested_at),
                            plural(p.devices.len(), "device", "devices")
                        ),
                        false,
                    )
                    .child(
                        ui::button(("teams-reject", i), "Reject", ButtonKind::Ghost, !busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.hub.update(cx, |h, cx| h.reject(&t1, gid, cx))
                            })),
                    )
                    .child(
                        ui::button(
                            ("teams-approve", i),
                            if busy { "Approving…" } else { "Approve" },
                            ButtonKind::Primary,
                            !busy && !keyless,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hub.update(cx, |h, cx| h.approve(&t2, gid, cx))
                        })),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .pl(ui::sp(50.))
                        .pr(ui::sp(10.))
                        .pb(ui::sp(8.))
                        .children(p.devices.iter().map(device_line)),
                );
        }
        let mut page = Self::page().child(ui::page_title(
            "Waiting for you",
            Some(
                "Approving sends the team key from this device to each of theirs. The server cannot do it."
                    .into_any_element(),
            ),
        ));
        if pending.is_empty() {
            page = page.child(Self::footnote("Nobody is waiting. New requests show here."));
        } else {
            page = page.child(Self::card().child(rows)).child(Self::footnote(
                "Check a device you do not know with its owner before you approve. Its code is at the right.",
            ));
        }
        if keyless {
            page = page.child(Self::footnote(
                "This device holds no team key yet, so it can't approve anyone.",
            ));
        }
        Self::scrolling("teams-pending-page", page)
    }

    fn render_devices(&self, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let mine = hub.device_id().unwrap_or_default().to_string();
        let devices = hub.my_devices.clone();
        let code_text = self.device_code.read(cx).text().to_string();
        let code = hub::read_device_code(&code_text);
        let busy = code
            .as_ref()
            .is_some_and(|c| hub.busy.contains(&format!("device:{}", c.id)));
        let known = code
            .as_ref()
            .is_some_and(|c| devices.iter().any(|d| d.device_id == c.id));
        let mut rows = Self::rows();
        for d in &devices {
            let this_one = d.device_id == mine;
            rows = rows.child(
                Self::person_row(
                    icon("computer", 16.).text_color(theme::text_subtle()),
                    crate::redact::text(&d.name).into_owned(),
                    format!(
                        "{} · added {}",
                        fingerprint(&d.device_id),
                        when(d.created_at)
                    ),
                    false,
                )
                .when(this_one, |r| {
                    r.child(ui::badge("This device", Tone::Neutral))
                }),
            );
        }
        let page = Self::page()
            .child(ui::page_title(
                "Your devices",
                Some(
                    "Every device signs what it sends, and holds its own copy of each team key."
                        .into_any_element(),
                ),
            ))
            .child(Self::card().child(rows))
            .child(ui::text(Type::Heading, theme::text(), "Add a device"))
            .child(ui::text(
                Type::Body,
                theme::text_subtle(),
                "On the new device, open Team: it shows a code. Paste it here to vouch for that device. Your teams' admins then give it the keys.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(ui::field().flex_1().child(self.device_code.clone()))
                    .child(
                        ui::button(
                            "teams-approve-device",
                            if busy { "Approving…" } else { "Approve" },
                            ButtonKind::Primary,
                            code.is_some() && !busy && !known,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let text = this.device_code.read(cx).text().to_string();
                            if let Some(code) = hub::read_device_code(&text) {
                                this.hub.update(cx, |h, cx| h.approve_device(&code, cx));
                                this.device_code.update(cx, |i, cx| i.set_text("", cx));
                            }
                        })),
                    ),
            )
            .children(code.map(|c| {
                ui::text(
                    Type::Caption,
                    if known {
                        theme::text_faint()
                    } else {
                        theme::text_subtle()
                    },
                    if known {
                        format!(
                            "{} is one of your devices already.",
                            crate::redact::text(&c.name)
                        )
                    } else {
                        format!(
                            "\"{}\", device {}. Check the new device shows the same code.",
                            crate::redact::text(&c.name),
                            fingerprint(&c.id)
                        )
                    },
                )
            }))
            .when(
                !code_text.trim().is_empty() && hub::read_device_code(&code_text).is_none(),
                |d| {
                    d.child(ui::text(
                        Type::Caption,
                        theme::attention().into(),
                        "That is not a device code. Copy the whole line the new device shows.",
                    ))
                },
            );
        Self::scrolling("teams-devices-page", page)
    }

    // ---- the chat -----------------------------------------------------------------------

    /// The channel column: channels, people, and a word on names.
    fn render_column(
        &self,
        team_id: &str,
        channel_id: &str,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return div().into_any_element();
        };
        let admin = t.role.is_admin() && !t.keyless;
        let channels: Vec<(String, Option<String>, Option<String>)> = t
            .channels
            .iter()
            .map(|c| {
                let unread = hub::unread_label(hub.unread(team_id, &c.id));
                (c.id.clone(), c.name.clone(), unread)
            })
            .collect();
        let here = hub
            .channels
            .get(&hub::channel_key(team_id, channel_id))
            .map(|c| c.here.as_slice())
            .unwrap_or_default();
        let people: Vec<(String, bool)> = hub::here_now(&t.detail.members, here)
            .into_iter()
            .map(|(m, on)| (m.login.clone(), on))
            .collect();
        let making = hub.busy.contains(&format!("channel:{team_id}"));
        let tid = team_id.to_string();
        let mut list =
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(2.))
                .child(Self::column_title("Channels").when(admin, |d| {
                    d.child(
                        ui::icon_button("teams-add-channel", "add01", 12.)
                            .tooltip(ui::tip("New channel"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.adding_channel = !this.adding_channel;
                                if this.adding_channel {
                                    let handle = this.channel_name.read(cx).focus_handle(cx);
                                    window.focus(&handle, cx);
                                }
                                cx.notify();
                            })),
                    )
                }));
        for (i, (cid, cname, unread)) in channels.into_iter().enumerate() {
            let on = cid == channel_id;
            let (t2, c2) = (tid.clone(), cid.clone());
            list = list.child(
                div()
                    .id(("teams-channel", i))
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .h(px(ui::INPUT_H))
                    .px(ui::sp(8.))
                    .rounded(ui::rad(ui::RADIUS))
                    .ty(Type::Body)
                    .cursor_pointer()
                    .text_color(if on {
                        theme::content()
                    } else {
                        theme::text_subtle()
                    })
                    .when(on, |d| d.bg(theme::fill_selected()))
                    .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                    .child(icon("comment01", 14.).text_color(if on {
                        theme::text_subtle()
                    } else {
                        theme::text_faint()
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(cname.unwrap_or_else(|| "locked channel".into())),
                    )
                    .children(unread.map(|n| {
                        ui::text(Type::Caption, theme::text_subtle(), n)
                            .flex_none()
                            .font_family(theme::FONT_MONO)
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(Pane::Channel(t2.clone(), c2.clone()), window, cx)
                    })),
            );
        }
        if self.adding_channel || making {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .h(px(ui::INPUT_H))
                    .px(ui::sp(8.))
                    .rounded(ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .ty(Type::Body)
                    .child(if making {
                        ui::spinner("teams-making-channel", 12., theme::text_faint())
                            .into_any_element()
                    } else {
                        icon("comment01", 14.)
                            .text_color(theme::text_faint())
                            .into_any_element()
                    })
                    .child(div().flex_1().min_w_0().child(self.channel_name.clone())),
            );
        }
        list = list
            .child(div().h(px(ui::sp(8.).as_f32())))
            .child(Self::column_title("Here now"));
        for (login, on) in people {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .h(px(ui::BUTTON_H))
                    .px(ui::sp(8.))
                    .ty(Type::Body)
                    .text_color(if on {
                        theme::text_muted()
                    } else {
                        theme::text_subtle()
                    })
                    .child(faces.avatar(&login, 20.))
                    .child(div().flex_1().min_w_0().truncate().child(login))
                    .child(div().flex_none().size(px(6.)).rounded_full().bg(if on {
                        theme::success().into()
                    } else {
                        theme::text_faint()
                    })),
            );
        }
        div()
            .id("teams-column")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(COLUMN_W))
            .px(ui::sp(8.))
            .pt(ui::sp(8.))
            .pb(ui::sp(12.))
            .border_r_1()
            .border_color(theme::border_subtle())
            .overflow_y_scroll()
            .child(list)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(6.))
                    .px(ui::sp(8.))
                    .pt(ui::sp(12.))
                    .ty(Type::Caption)
                    .text_color(theme::text_faint())
                    .child(icon("square-lock02", 12.).text_color(theme::text_faint()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .child("Channel names are encrypted too"),
                    ),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_message(
        &self,
        index: usize,
        m: &Msg,
        compact: bool,
        quoted: Option<(usize, &Msg)>,
        mark: Mark,
        team_channel: (&str, &str),
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group: SharedString = format!("teams-msg-{index}").into();
        let id = m.id.clone();
        let lit = !mark.in_range
            && (self.flash.as_deref() == Some(m.id.as_str())
                || self.reply_to.as_deref() == Some(m.id.as_str()));
        let quote = m.body.reply_to.as_ref().map(|target| match quoted {
            Some((qi, q)) => {
                let qid = q.id.clone();
                div()
                    .id(("teams-quote", index))
                    .flex()
                    .items_center()
                    .gap(ui::sp(6.))
                    .min_w_0()
                    .pb(ui::sp(2.))
                    .cursor_pointer()
                    .child(icon("arrow-turn-forward", 12.).text_color(theme::text_faint()))
                    .child(
                        ui::text(Type::Caption, theme::text_subtle(), q.login.clone())
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_faint())
                            .hover(|d| d.text_color(theme::text()))
                            .child(snippet(&q.body.text)),
                    )
                    .tooltip(ui::tip("Jump to the message"))
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(qi, qid.clone(), cx)))
                    .into_any_element()
            }
            None => div()
                .flex()
                .items_center()
                .gap(ui::sp(6.))
                .pb(ui::sp(2.))
                .child(icon("arrow-turn-forward", 12.).text_color(theme::text_faint()))
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    if target.is_empty() {
                        "a reply"
                    } else {
                        "Replying to a message not loaded here"
                    },
                ))
                .into_any_element(),
        });
        let compact = compact && quote.is_none() && mark.number.is_none();
        let kind = match m.body.kind.as_str() {
            "text" => None,
            "bot" => Some("bot"),
            "decision" => Some("decision"),
            "decision_accept" => Some("accepted"),
            other => Some(other),
        }
        .map(|k| ui::badge(k.to_string(), Tone::Info));
        let delivery = self.delivery(index, m, team_channel, cx);
        let text_color = if m.delivery == Delivery::Sending {
            theme::text_subtle()
        } else {
            theme::text()
        };
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .children(quote)
            .when(!compact, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(
                            ui::text(Type::Body, theme::content(), m.login.clone())
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .children(mark.number.map(|n| {
                            ui::text(Type::Caption, theme::text_faint(), n.to_string())
                                .font_family(theme::FONT_MONO)
                        }))
                        .child(
                            ui::text(Type::Caption, theme::text_faint(), clock(m.at))
                                .font_family(theme::FONT_MONO),
                        )
                        .children(kind),
                )
            })
            .child(
                div()
                    .ty(Type::Body)
                    .text_color(text_color)
                    .child(m.body.text.clone()),
            )
            .children(delivery);
        div()
            .id(("teams-message", index))
            .group(group.clone())
            .relative()
            .flex()
            .items_start()
            .gap(ui::sp(12.))
            .px(ui::sp(16.))
            .pt(ui::sp(if compact { 0. } else { 8. }))
            .pb(ui::sp(if compact { 2. } else { 6. }))
            .hover(|d| d.bg(theme::fill_hover()))
            .when(lit, |d| d.bg(theme::fill_selected()))
            .when(mark.in_range, Self::in_range)
            .child(if compact {
                div()
                    .w(px(AVATAR))
                    .flex_none()
                    .invisible()
                    .group_hover(group.clone(), |s| s.visible())
                    .child(
                        ui::text(Type::Caption, theme::text_faint(), clock(m.at))
                            .font_family(theme::FONT_MONO)
                            .pt(ui::sp(2.))
                            .whitespace_nowrap(),
                    )
            } else {
                faces.avatar(&m.login, AVATAR).mt(px(2.))
            })
            .child(body)
            .when(m.delivery == Delivery::Sent, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .right(ui::sp(12.))
                        .invisible()
                        .group_hover(group, |s| s.visible())
                        .rounded(ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::raised())
                        .child(
                            ui::icon_button(("teams-reply", index), "arrow-turn-backward", 13.)
                                .tooltip(ui::tip("Reply"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reply_to = Some(id.clone());
                                    let handle = this.composer.read(cx).focus_handle(cx);
                                    window.focus(&handle, cx);
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    /// "sending…", or why it was not sent and a way to send it again.
    fn delivery(
        &self,
        index: usize,
        m: &Msg,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match &m.delivery {
            Delivery::Sent => None,
            Delivery::Sending => {
                Some(ui::text(Type::Caption, theme::text_faint(), "sending…").into_any_element())
            }
            Delivery::Failed(why) => {
                let (t, c, mid) = (
                    team_channel.0.to_string(),
                    team_channel.1.to_string(),
                    m.id.clone(),
                );
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(6.))
                        .child(ui::text(
                            Type::Caption,
                            theme::error().into(),
                            format!("not sent: {why}"),
                        ))
                        .child(
                            div()
                                .id(("teams-resend", index))
                                .cursor_pointer()
                                .ty(Type::Caption)
                                .text_color(theme::link())
                                .child("Send again")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.hub.update(cx, |h, cx| h.resend(&t, &c, &mid, cx))
                                })),
                        )
                        .into_any_element(),
                )
            }
        }
    }

    /// "Rik added a new device, rik-thinkpad", where it happened in the chat.
    fn render_notice(&self, i: usize, n: &Notice, cx: &mut Context<Self>) -> AnyElement {
        let id = n.device_id.clone();
        let verified = self.hub.read(cx).verified(&n.device_id);
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .mx(ui::sp(16.))
            .my(ui::sp(8.))
            .pl(ui::sp(12.))
            .pr(ui::sp(4.))
            .py(ui::sp(4.))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_subtle())
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::text_muted())
            .child(icon("computer", 14.).text_color(theme::text_subtle()))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .child(format!("{} added a new device,\u{a0}", n.login))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(crate::redact::text(&n.device).into_owned()),
                    )
                    .child(".\u{a0}It can read from here on."),
            )
            .when(verified, |d| d.child(ui::badge("Checked", Tone::Success)))
            .child(
                ui::button(
                    ("teams-notice-compare", i),
                    "Compare",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.side = Some(Side::Compare(Some(id.clone())));
                    this.mismatch = None;
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn render_composer(
        &self,
        can_send: bool,
        connected: bool,
        bot_hint: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reply = self.reply_to.as_ref().and_then(|id| {
            let Pane::Channel(t, c) = &self.pane else {
                return None;
            };
            self.hub
                .read(cx)
                .channels
                .get(&hub::channel_key(t, c))?
                .messages
                .iter()
                .find(|m| m.id == *id)
                .cloned()
        });
        let typed = !self.composer.read(cx).text().trim().is_empty();
        let ready = typed && can_send;
        let reply_bar = reply.map(|r| {
            Self::above_row()
                .child(icon("arrow-turn-forward", 12.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(div().flex_none().child("Replying to\u{a0}"))
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .child(r.login.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(format!(":\u{a0}{}", snippet(&r.body.text))),
                        ),
                )
                .child(
                    ui::icon_button("teams-reply-cancel", "cancel01", 12.)
                        .tooltip(ui::tip("Cancel reply (Esc)"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reply_to = None;
                            cx.notify();
                        })),
                )
        });
        let above = reply_bar.is_some() || bot_hint.is_some();
        let square = |id: &'static str| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(ui::CONTROL_H))
                .rounded(ui::nested(ui::RADIUS_XL, 8.))
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .px(ui::sp(16.))
            .pb(ui::sp(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(ui::rad(ui::RADIUS_XL))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::raised())
                    .when(above, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(ui::sp(4.))
                                .px(ui::sp(12.))
                                .pt(ui::sp(12.))
                                .children(reply_bar)
                                .children(bot_hint),
                        )
                    })
                    .child(
                        div()
                            .px(ui::sp(12.))
                            .pt(ui::sp(10.))
                            .pb(ui::sp(6.))
                            .text_size(px(ui::TEXT_LG))
                            .child(self.composer.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(4.))
                            .px(ui::sp(8.))
                            .pb(ui::sp(8.))
                            .child(
                                square("teams-attach")
                                    .bg(theme::selection())
                                    .opacity(0.5)
                                    .tooltip(ui::tip("Files come in a later version"))
                                    .child(icon("add01", 14.).text_color(theme::text_subtle())),
                            )
                            .child(self.render_bot_chip(cx))
                            .child(div().flex_1())
                            .when(ready, |d| {
                                d.child(
                                    ui::text(Type::Caption, theme::text_faint(), "Enter to send")
                                        .pr(ui::sp(8.)),
                                )
                            })
                            .child(
                                square("teams-send")
                                    .bg(if ready {
                                        theme::content()
                                    } else {
                                        theme::fill_selected()
                                    })
                                    .when(ready, |d| d.cursor_pointer())
                                    .tooltip(ui::tip("Send (Enter)"))
                                    .child(icon("arrow-up01", 14.).text_color(if ready {
                                        theme::background()
                                    } else {
                                        theme::text_faint()
                                    }))
                                    .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                            ),
                    ),
            )
            .when(!can_send || !connected, |d| {
                d.child(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        if can_send {
                            "Not connected right now. What you send goes out once it is back."
                        } else {
                            "You can write here once this device holds the team key."
                        },
                    )
                    .pt(ui::sp(6.))
                    .px(ui::sp(4.)),
                )
            })
            .into_any_element()
    }

    /// A line over the composer's text: the message it replies to, or what
    /// @bot will read.
    fn above_row() -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .h(px(ui::CONTROL_H))
            .px(ui::sp(8.))
            .rounded(ui::rad(ui::RADIUS))
            .bg(theme::fill_hover())
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::text_muted())
    }

    fn render_channel(
        &mut self,
        team_id: &str,
        channel_id: &str,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t): Option<&TeamState> = hub.team.get(team_id) else {
            return ui::loading("teams-channel-loading", "Reading the team…").into_any_element();
        };
        let key = hub::channel_key(team_id, channel_id);
        let c: Option<&ChannelState> = hub.channels.get(&key);
        let name = t
            .channels
            .iter()
            .find(|ch| ch.id == channel_id)
            .and_then(|ch| ch.name.clone())
            .unwrap_or_else(|| "locked channel".into());
        let status = c.map_or(socket::Status::Connecting, |c| c.status.clone());
        let messages: Vec<Msg> = c.map(|c| c.messages.clone()).unwrap_or_default();
        let loading = c.is_none_or(|c| c.loading);
        let (hidden, locked) = c.map_or((0, 0), |c| (c.hidden, c.locked()));
        let keyless = t.keyless;
        let joined = hub.joined.contains(team_id);
        let team_name = t.detail.name.clone();
        // Each notice goes where its device was added among the messages.
        let added: HashMap<String, u64> = t
            .devices
            .values()
            .flatten()
            .map(|d| (d.device_id.clone(), d.created_at))
            .collect();
        let notices: Vec<(Notice, u64)> = hub
            .notices
            .iter()
            .filter(|n| n.team_id == team_id)
            .map(|n| {
                (
                    n.clone(),
                    added.get(&n.device_id).copied().unwrap_or(u64::MAX),
                )
            })
            .collect();

        let mut bot = self.bot_state(&messages, team_id, channel_id, cx);

        // Keep the newest in view when it arrives and the list was at its
        // bottom. A run of @bot counts, and so does its answer replacing it.
        let seen = messages.len() * 2 + bot.runs.len();
        if seen != self.seen_count {
            let max = self.scroll.max_offset().y;
            let at_bottom = -self.scroll.offset().y >= max - px(48.);
            if self.seen_count == 0 || at_bottom {
                self.scroll.scroll_to_bottom();
            }
            self.seen_count = seen;
        }

        // The list, laid out first so a quote knows its target's row.
        let mut lines: Vec<Line> = Vec::new();
        let mut day = None;
        let mut placed = vec![false; notices.len()];
        let mut runs_placed = vec![false; bot.runs.len()];
        for (i, m) in messages.iter().enumerate() {
            // An acceptance shows as the "Decided" line under its answer.
            if m.body.kind == "decision_accept" {
                continue;
            }
            for (n, (notice, at)) in notices.iter().enumerate() {
                if !placed[n] && *at <= m.at {
                    placed[n] = true;
                    lines.push(Line::Notice(notice.clone()));
                }
            }
            if let Some(d) = crate::platform::local_time(m.at).map(|t| (t.year, t.month, t.day))
                && day != Some(d)
            {
                day = Some(d);
                lines.push(Line::Day(day_label(m.at)));
            }
            if joined && i == 0 {
                lines.push(Line::Joined);
            }
            lines.push(Line::Message(i));
            if bot.decided.contains_key(&m.id) {
                lines.push(Line::Decided(i));
            }
            for (r, run) in bot.runs.iter().enumerate() {
                if run.ask_id == m.id {
                    runs_placed[r] = true;
                    lines.push(Line::Run(r));
                }
            }
        }
        if joined && messages.is_empty() {
            lines.push(Line::Joined);
        }
        for (r, placed) in runs_placed.into_iter().enumerate() {
            if !placed {
                lines.push(Line::Run(r));
            }
        }
        for (n, (notice, _)) in notices.iter().enumerate() {
            if !placed[n] {
                lines.push(Line::Notice(notice.clone()));
            }
        }

        // One room-filling spacer and one note go first.
        const LEAD: usize = 2;
        let index_of: HashMap<&str, usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(row, l)| match l {
                Line::Message(i) => Some((messages[*i].id.as_str(), row + LEAD)),
                _ => None,
            })
            .collect();
        if let Some(id) = self.pending_jump.clone()
            && let Some(&row) = index_of.get(id.as_str())
        {
            self.pending_jump = None;
            self.jump_to(row, id, cx);
        }

        let mut note = Vec::new();
        if hidden > 0 {
            note.push(format!(
                "{hidden} message{} did not check out and {} hidden.",
                if hidden == 1 { "" } else { "s" },
                if hidden == 1 { "is" } else { "are" }
            ));
        }
        if locked > 0 {
            note.push(format!(
                "{locked} older message{} {} sealed with a key this device doesn't hold.",
                if locked == 1 { "" } else { "s" },
                if locked == 1 { "is" } else { "are" }
            ));
        }
        let first = if loading && messages.is_empty() {
            ui::loading("teams-history", "Reading the channel…").into_any_element()
        } else if messages.is_empty() && hidden + locked == 0 && !joined {
            ui::empty_state(
                "comment01",
                &format!("This is the start of {name}"),
                "Say hello. Only this team's devices can read what is said here.",
                None,
            )
            .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(4.))
                .px(ui::sp(16.))
                .children(
                    note.into_iter()
                        .map(|n| ui::text(Type::Caption, theme::attention().into(), n)),
                )
                .into_any_element()
        };
        let mut rows: Vec<AnyElement> = vec![div().flex_1().into_any_element(), first];
        for (li, line) in lines.iter().enumerate() {
            let el = match line {
                Line::Day(label) => {
                    ui::text(Type::Overline, theme::text_faint(), label.to_uppercase())
                        .px(ui::sp(16.))
                        .pt(ui::sp(12.))
                        .pb(ui::sp(4.))
                        .into_any_element()
                }
                Line::Notice(n) => self.render_notice(li, n, cx),
                Line::Joined => div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(6.))
                    .px(ui::sp(16.))
                    .py(ui::sp(8.))
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_subtle())
                    .child(icon("tick02", 12.).text_color(theme::success()))
                    .child(format!(
                        "An admin approved {}. You can read {team_name} from here on, and its history.",
                        crate::redact::text(&hub::device_name())
                    ))
                    .into_any_element(),
                Line::Message(i) => {
                    let i = *i;
                    let m = &messages[i];
                    let compact = match lines.get(li.wrapping_sub(1)) {
                        Some(Line::Message(p)) => {
                            let p = &messages[*p];
                            p.github_id == m.github_id
                                && m.at.saturating_sub(p.at) < GROUP_SECS
                                && p.body.kind == m.body.kind
                        }
                        _ => false,
                    };
                    let quoted = m.body.reply_to.as_deref().and_then(|r| {
                        Some((*index_of.get(r)?, messages.iter().find(|q| q.id == r)?))
                    });
                    if m.body.kind == "bot" {
                        self.render_bot_message(
                            li + LEAD,
                            m,
                            &bot,
                            &index_of,
                            (team_id, channel_id),
                            cx,
                        )
                    } else {
                        let mark = Mark {
                            in_range: bot.draft.contains(m.id.as_str()),
                            number: bot.numbers.get(m.id.as_str()).copied(),
                        };
                        self.render_message(
                            li + LEAD,
                            m,
                            compact,
                            quoted,
                            mark,
                            (team_id, channel_id),
                            faces,
                            cx,
                        )
                    }
                }
                Line::Decided(i) => self.render_decided(li, &messages[*i], &bot, team_id, cx),
                Line::Run(r) => self.render_run(li, &bot.runs[*r], &bot, cx),
            };
            rows.push(el);
        }
        let list = div()
            .id(SharedString::from(format!(
                "teams-messages-{team_id}-{channel_id}"
            )))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .pb(ui::sp(12.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);

        let can_send = !keyless;
        let connected = status == socket::Status::Connected;
        let status_line = match &status {
            socket::Status::Connected => None,
            socket::Status::Connecting => Some(("connecting…".to_string(), Tone::Neutral)),
            socket::Status::Waiting(_) => Some(("reconnecting…".to_string(), Tone::Warning)),
            socket::Status::Refused(why) => Some((why.clone(), Tone::Danger)),
        };
        let center = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when_some(
                status_line.filter(|_| !messages.is_empty()),
                |d, (w, tone)| {
                    d.child(
                        div()
                            .flex()
                            .justify_end()
                            .px(ui::sp(16.))
                            .pt(ui::sp(6.))
                            .child(ui::status_dot(w, tone)),
                    )
                },
            )
            .child(list)
            .child(self.render_composer(can_send, connected, bot.hint.take(), cx));

        let column = self.render_column(team_id, channel_id, faces, cx);
        let side = self
            .side
            .clone()
            .map(|s| self.render_side(team_id, s, faces, cx));
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(column)
            .child(center)
            .children(side)
            .into_any_element()
    }

    // ---- the right panel ----------------------------------------------------------------

    fn render_side(
        &self,
        team_id: &str,
        side: Side,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, body) = match &side {
            Side::Encryption => (
                "Encryption".to_string(),
                self.render_encryption(team_id, cx),
            ),
            Side::Compare(None) => (
                "Compare a device".to_string(),
                self.render_pick_device(team_id, faces, cx),
            ),
            Side::Compare(Some(device)) => self.render_compare(team_id, device, cx),
        };
        div()
            .id("teams-side")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(PANEL_W))
            .border_l_1()
            .border_color(theme::border_subtle())
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(ui::BAR_H))
                    .pl(ui::sp(16.))
                    .pr(ui::sp(8.))
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Body)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(title),
                    )
                    .child(
                        ui::icon_button("teams-side-close", "cancel01", 14.)
                            .tooltip(ui::tip("Close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side = None;
                                this.mismatch = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(16.))
                    .px(ui::sp(16.))
                    .pt(ui::sp(8.))
                    .pb(ui::sp(16.))
                    .child(body),
            )
            .into_any_element()
    }

    /// "Encrypted. The server can't read it.", with what it does see.
    fn render_encryption(&self, team_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let host = self
            .teams
            .read(cx)
            .server_host()
            .unwrap_or_else(|| "The server".into());
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return div().into_any_element();
        };
        let epoch = t.detail.epoch;
        let people = t.detail.members.len();
        let devices: usize = t.devices.values().map(Vec::len).sum();
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(8.))
                    .child(ui::text(
                        Type::Heading,
                        theme::text(),
                        "Encrypted. The server can't read it.",
                    ))
                    .child(ui::text(
                        Type::Body,
                        theme::text_subtle(),
                        format!(
                            "Messages and channel names are locked on your devices before they leave. {host} only passes them on."
                        ),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(Self::fact("Team key", format!("Epoch {epoch}")))
                    .child(Self::fact(
                        "Held by",
                        format!(
                            "{} · {}",
                            plural(people, "person", "people"),
                            plural(devices, "device", "devices")
                        ),
                    ))
                    .child(Self::fact("The server sees", "Who is in the team, and when"))
                    .child(Self::fact("The server can't see", "Messages, names, files")),
            )
            .child(
                div()
                    .flex()
                    .gap(ui::sp(8.))
                    .child(
                        ui::button("teams-see-devices", "See devices", ButtonKind::Outline, true)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.hub.update(cx, |h, cx| h.load_my_devices(cx));
                                this.show(Pane::Devices, window, cx)
                            })),
                    )
                    .child(
                        ui::button("teams-compare-any", "Compare a device", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side = Some(Side::Compare(None));
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Every device of the team's people but this one, to pick one to compare.
    fn render_pick_device(
        &self,
        team_id: &str,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return div().into_any_element();
        };
        let mine = hub.device_id().unwrap_or_default().to_string();
        let mut devices: Vec<(String, DeviceRow, bool)> = t
            .detail
            .members
            .iter()
            .flat_map(|m| {
                t.devices
                    .get(&m.github_id)
                    .into_iter()
                    .flatten()
                    .filter(|d| d.device_id != mine)
                    .map(|d| (m.login.clone(), d.clone(), hub.verified(&d.device_id)))
            })
            .collect();
        devices.sort_by_key(|(_, _, verified)| *verified);
        let mut list = div().flex().flex_col().gap(ui::sp(2.)).child(ui::text(
            Type::Body,
            theme::text_subtle(),
            "Pick a device. Its owner opens Compare on it, and you both read the numbers.",
        ));
        if devices.is_empty() {
            list = list.child(Self::footnote("No other devices in this team yet."));
        }
        for (i, (login, d, verified)) in devices.into_iter().enumerate() {
            let id = d.device_id.clone();
            list = list.child(
                div()
                    .id(("teams-pick-device", i))
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .h(px(ui::INPUT_H))
                    .px(ui::sp(8.))
                    .rounded(ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .ty(Type::Body)
                    .child(faces.avatar(&login, 20.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text())
                            .child(format!("{login} · {}", crate::redact::text(&d.name))),
                    )
                    .when(verified, |r| {
                        r.child(icon("tick02", 14.).text_color(theme::success()))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.side = Some(Side::Compare(Some(id.clone())));
                        this.mismatch = None;
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    /// The numbers both devices show, and the same as a QR code.
    fn render_compare(
        &self,
        team_id: &str,
        device_id: &str,
        cx: &mut Context<Self>,
    ) -> (String, AnyElement) {
        let hub = self.hub.read(cx);
        let found = hub.team.get(team_id).and_then(|t| {
            t.devices.iter().find_map(|(gid, ds)| {
                let d = ds.iter().find(|d| d.device_id == device_id)?;
                Some((t.login(*gid), d.clone()))
            })
        });
        let Some((login, device)) = found else {
            return (
                "Compare a device".into(),
                Self::footnote("That device is not in this team any more.").into_any_element(),
            );
        };
        let name = crate::redact::text(&device.name).into_owned();
        let numbers = hub
            .sig_pub()
            .and_then(|mine| super::crypto::compare_numbers(mine, &device.sig_pub));
        let verified = hub.verified(device_id);
        let mismatch = self.mismatch.as_deref() == Some(device_id);
        let title = format!("Compare {name}");
        let Some(numbers) = numbers else {
            return (
                title,
                Self::footnote(
                    "This device's keys could not be read, so there is nothing to compare.",
                )
                .into_any_element(),
            );
        };
        let cell = |n: &String| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .h(px(ui::INPUT_H))
                .rounded(ui::rad(ui::RADIUS))
                .border_1()
                .border_color(theme::border_subtle())
                .bg(theme::sunk())
                .font_family(theme::FONT_MONO)
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text())
                .child(n.clone())
        };
        let grid = div()
            .flex()
            .flex_col()
            .gap(ui::sp(4.))
            .child(
                div()
                    .flex()
                    .gap(ui::sp(4.))
                    .children(numbers[..3].iter().map(cell)),
            )
            .child(
                div()
                    .flex()
                    .gap(ui::sp(4.))
                    .children(numbers[3..].iter().map(cell)),
            );
        let id1 = device_id.to_string();
        let id2 = device_id.to_string();
        let body = div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .child(ui::text(
                Type::Body,
                theme::text_subtle(),
                format!(
                    "Ask {login} to open Compare on {name}. If both show the same numbers, it is theirs."
                ),
            ))
            .child(grid)
            .child(div().flex().justify_center().child(render_qr(&numbers.concat())))
            .child(Self::note(
                "view",
                "Or point the other device's camera at this.",
            ))
            .children(if verified {
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .ty(Type::Label)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::success())
                        .child(icon("tick02", 14.).text_color(theme::success()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(format!("You checked {name} with {login}. It is theirs.")),
                        )
                        .into_any_element(),
                )
            } else if mismatch {
                Some(

                ui::text(
                    Type::Label,
                    theme::error().into(),
                    format!(
                        "The numbers differ: {name} may not be {login}'s. Tell {login} and a team admin, and read its messages with care."
                    ),
                )
                .font_weight(FontWeight::NORMAL)
                .into_any_element(),
                )
            } else {
                None
            })
            .child(
                div()
                    .flex()
                    .gap(ui::sp(8.))
                    .child(
                        ui::button_with(
                            "teams-match",
                            Some("tick02"),
                            "They match",
                            ButtonKind::Primary,
                            !verified,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mismatch = None;
                            let id = id1.clone();
                            this.hub.update(cx, |h, cx| {
                                h.set_verified(&id, true, cx);
                                h.dismiss_notice(&id, cx);
                            });
                        })),
                    )
                    .child(
                        ui::button_with(
                            "teams-no-match",
                            Some("cancel01"),
                            "They don't",
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mismatch = Some(id2.clone());
                            let id = id2.clone();
                            this.hub.update(cx, |h, cx| h.set_verified(&id, false, cx));
                        })),
                    ),
            );
        (title, body.into_any_element())
    }

    // ---- states before the teams ---------------------------------------------------------

    fn centered(body: impl IntoElement) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .px(ui::sp(ui::PAGE_PAD))
            .child(div().max_w(px(460.)).child(body))
            .into_any_element()
    }

    fn render_unendorsed(&self, code: &str, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let code_owned = code.to_string();
        let copied = self.copied;
        Self::scrolling(
            "teams-unendorsed",
            Self::page()
                .child(ui::page_title(
                    "Approve this device from your other device",
                    Some(
                        "You use Team on another device already. There, open the team's menu › Your devices and paste this code. Only a device of yours can let this one in."
                            .into_any_element(),
                    ),
                ))
                .child(
                    Self::card().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(12.))
                            .p(ui::sp(16.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(ui::sp(8.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .px(ui::sp(10.))
                                            .py(ui::sp(6.))
                                            .rounded(ui::rad(ui::RADIUS))
                                            .bg(theme::sunk())
                                            .border_1()
                                            .border_color(theme::border())
                                            .font_family(theme::FONT_MONO)
                                            .text_size(px(ui::TEXT_SM))
                                            .text_color(theme::text_muted())
                                            .child(code.to_string()),
                                    )
                                    .child(
                                        ui::button_with(
                                            "teams-copy-code",
                                            Some(if copied { "tick02" } else { "copy01" }),
                                            if copied { "Copied" } else { "Copy" },
                                            ButtonKind::Outline,
                                            true,
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                code_owned.clone(),
                                            ));
                                            this.copied = true;
                                            cx.notify();
                                        })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(ui::sp(8.))
                                    .child(ui::spinner(
                                        "teams-endorse-wait",
                                        14.,
                                        theme::text_subtle(),
                                    ))
                                    .child(
                                        ui::text(
                                            Type::Label,
                                            theme::text_subtle(),
                                            format!(
                                                "Waiting for approval · this device's code is {}",
                                                fingerprint(id)
                                            ),
                                        )
                                        .font_weight(FontWeight::NORMAL),
                                    ),
                            ),
                    ),
                ),
        )
    }

    /// The sheet once this device is registered.
    fn render_ready(&mut self, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let no_teams = hub.teams.is_empty();
        let has_invites = !hub.invites.is_empty();
        let asked = hub.asked();
        let error = hub.error.clone();
        let info = hub.info.clone();
        let unreachable = match &hub.load {
            Load::Unreachable(e) => Some(e.clone()),
            _ => None,
        };
        let keyless = |t: &str| hub.team.get(t).is_some_and(|t| t.keyless);
        let admins_of = |t: &str| -> Vec<String> {
            hub.team
                .get(t)
                .map(|t| {
                    t.detail
                        .members
                        .iter()
                        .filter(|m| m.role.is_admin())
                        .map(|m| m.login.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        let name_of = |t: &str| {
            hub.teams
                .iter()
                .find(|r| r.team_id == t)
                .map(|r| r.name.clone())
                .unwrap_or_default()
        };
        // A device without the team key waits, whatever was picked.
        let waiting_on = self
            .selected_team()
            .filter(|t| keyless(t))
            .map(|t| (t.to_string(), name_of(t), admins_of(t)));
        let inviters = hub.inviters.clone();

        let (header_team, header_channel, body): (Option<String>, Option<String>, AnyElement) =
            match self.pane.clone() {
                _ if waiting_on.is_some() => {
                    let (t, name, admins) = waiting_on.expect("checked");
                    let subtitle =
                        "This device is new to the team. One step is left, and it is not yours.";
                    (
                        Some(t),
                        None,
                        self.render_waiting(&name, subtitle, admins, faces),
                    )
                }
                Pane::Channel(t, c) => {
                    let name = self
                        .hub
                        .read(cx)
                        .team
                        .get(&t)
                        .and_then(|s| s.channels.iter().find(|ch| ch.id == c))
                        .map(|ch| ch.name.clone().unwrap_or_else(|| "locked channel".into()));
                    let body = self.render_channel(&t, &c, faces, cx);
                    (Some(t), name, body)
                }
                Pane::Team(t) => (Some(t.clone()), None, self.render_team(&t, faces, cx)),
                Pane::Invite(t) => (Some(t.clone()), None, self.render_invite(&t, faces, cx)),
                Pane::Pending(t) => (Some(t.clone()), None, self.render_pending(&t, faces, cx)),
                Pane::Decisions(t) => (
                    Some(t.clone()),
                    Some("decisions".into()),
                    self.render_decisions(&t, faces, cx),
                ),
                Pane::Devices => {
                    let team = self
                        .last_team
                        .clone()
                        .filter(|t| !name_of(t).is_empty())
                        .or_else(|| self.hub.read(cx).teams.first().map(|r| r.team_id.clone()));
                    (team, None, self.render_devices(cx))
                }
                Pane::NewTeam => (None, None, self.render_new_team(cx)),
                Pane::Invites => (None, None, self.render_invites(faces, cx)),
                Pane::None if no_teams && has_invites => {
                    (None, None, self.render_invites(faces, cx))
                }
                Pane::None if no_teams && !asked.is_empty() => {
                    let (t, name) = asked[0].clone();
                    let who: Vec<String> = inviters.get(&t).cloned().into_iter().collect();
                    let subtitle =
                        "You accepted the invite. One step is left, and it is not yours.";
                    (None, None, self.render_waiting(&name, subtitle, who, faces))
                }
                Pane::None if no_teams => (None, None, self.render_new_team(cx)),
                Pane::None => (
                    None,
                    None,
                    Self::centered(ui::loading("teams-opening", "Opening the team…")),
                ),
            };
        let header = header_team.map(|t| self.render_header(&t, header_channel, faces, cx));
        let retry = ui::button_with(
            "teams-retry-banner",
            Some("refresh01"),
            "Try again",
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.hub.update(cx, |h, cx| h.retry(cx))));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .children(header)
            .children(unreachable.map(|e| {
                Self::banner(
                    "alert-circle",
                    Tone::Warning,
                    format!("Can't reach the Teams server: {}", crate::redact::text(&e)),
                )
                .child(retry)
            }))
            .children(info.map(|e| {
                Self::banner(
                    "tick02",
                    Tone::Success,
                    crate::redact::text(&e).into_owned(),
                )
                .child(
                    ui::icon_button("teams-info-dismiss", "cancel01", 12.)
                        .tooltip(ui::tip("Dismiss"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.hub.update(cx, |h, cx| {
                                h.info = None;
                                cx.notify();
                            })
                        })),
                )
            }))
            .children(error.map(|e| {
                Self::banner(
                    "alert-circle",
                    Tone::Warning,
                    crate::redact::text(&e).into_owned(),
                )
                .child(
                    ui::icon_button("teams-error-dismiss", "cancel01", 12.)
                        .tooltip(ui::tip("Dismiss"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.hub.update(cx, |h, cx| {
                                h.error = None;
                                cx.notify();
                            })
                        })),
                )
            }))
            .child(body)
            .into_any_element()
    }
}

impl Render for TeamsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drawn means shown: the unread counts of channels with no socket
        // here follow, at most once a minute.
        self.hub.update(cx, |h, cx| h.refresh_unread(false, cx));
        let teams = self.teams.read(cx);
        let enabled = teams.enabled();
        let faces = Faces::of(teams);
        let hub = self.hub.read(cx);
        let device = hub.device.clone();
        let load = hub.load.clone();
        let empty = hub.teams.is_empty() && hub.invites.is_empty() && hub.asked().is_empty();

        let retry = |id: &'static str, cx: &mut Context<Self>| {
            ui::button_with(
                id,
                Some("refresh01"),
                "Try again",
                ButtonKind::Outline,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.hub.update(cx, |h, cx| h.retry(cx))))
            .into_any_element()
        };

        let body: AnyElement = if !enabled {
            // The tab can stay open after a sign-out, or after the server took
            // the flag away.
            Self::centered(ui::empty_state(
                "ungroup-items",
                "Team is not on here",
                "Sign in with GitHub in Settings, with an account that has Teams.",
                Some(
                    ui::button(
                        "teams-open-settings",
                        "Open Settings",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamsViewEvent::OpenSettings)))
                    .into_any_element(),
                ),
            ))
        } else {
            match (&device, &load) {
                (DeviceState::Starting, _) => {
                    Self::centered(ui::loading("teams-starting", "Setting up this device…"))
                }
                (DeviceState::Unendorsed { code, id }, _) => {
                    let (code, id) = (code.clone(), id.clone());
                    self.render_unendorsed(&code, &id, cx)
                }
                (DeviceState::Failed(e), _) => Self::centered(ui::empty_state(
                    "alert-circle",
                    "Can't reach the Teams server",
                    &crate::redact::text(e),
                    Some(retry("teams-retry-device", cx)),
                )),
                (_, Load::Unreachable(e)) if empty => Self::centered(ui::empty_state(
                    "alert-circle",
                    "Can't reach the Teams server",
                    &crate::redact::text(e),
                    Some(retry("teams-retry-load", cx)),
                )),
                (_, Load::Loading) if empty => {
                    Self::centered(ui::loading("teams-loading", "Reading your teams…"))
                }
                _ => self.render_ready(&faces, cx),
            }
        };
        let menu = self.team_menu.then(|| self.render_team_menu(cx));

        div()
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            // No background: the window's card paints the sheet and rounds its
            // corners. A fill here would cover them, since its clip is square.
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(body)
            .when_some(menu, |d, menu| {
                d.child(
                    div()
                        .id("teams-menu-backdrop")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.team_menu = false;
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(ui::BAR_H - 4.))
                        .left(ui::sp(12.))
                        .child(menu),
                )
            })
    }
}

/// A device under a person waiting for approval: its name and its code.
fn device_line(d: &DeviceRow) -> gpui::Div {
    let phone = d.name.to_ascii_lowercase().contains("phone");
    div()
        .flex()
        .items_center()
        .gap(ui::sp(8.))
        .h(px(ui::BUTTON_H))
        .ty(Type::Label)
        .font_weight(FontWeight::NORMAL)
        .text_color(theme::text_muted())
        .child(
            icon(if phone { "signal-full02" } else { "computer" }, 14.)
                .text_color(theme::text_subtle()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(crate::redact::text(&d.name).into_owned()),
        )
        .child(
            ui::text(
                Type::Caption,
                theme::text_faint(),
                fingerprint(&d.device_id),
            )
            .font_family(theme::FONT_MONO),
        )
}

/// A QR code of `text` on white, as large as the panel's design.
fn render_qr(text: &str) -> gpui::Div {
    const SIDE: f32 = 132.;
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else {
        return div();
    };
    let width = code.width();
    let colors = code.to_colors();
    let cell = (SIDE / width as f32).floor();
    let mut rows = div()
        .flex()
        .flex_col()
        .p(ui::sp(8.))
        .rounded(ui::rad(ui::RADIUS_SM))
        .bg(gpui::white());
    for y in 0..width {
        let mut row = div().flex();
        for x in 0..width {
            let dark = colors[y * width + x] == qrcode::Color::Dark;
            row = row.child(div().size(px(cell)).when(dark, |d| d.bg(gpui::black())));
        }
        rows = rows.child(row);
    }
    rows
}

fn plural(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// A device's code, short enough to read out: the start and end of its ID.
fn fingerprint(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() <= 8 {
        return id.to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// A reply's quote of a message: its first line, cut short.
fn snippet(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > SNIPPET {
        format!("{}…", line.chars().take(SNIPPET).collect::<String>())
    } else if text.lines().count() > 1 {
        format!("{line} …")
    } else {
        line.to_string()
    }
}

fn clock(at: u64) -> String {
    crate::platform::local_time(at)
        .map(|t| format!("{:02}:{:02}", t.hour, t.minute))
        .unwrap_or_default()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How long ago, in words: "just now", "4 minutes ago", "yesterday".
fn ago(at: u64) -> String {
    ago_from(at, now())
}

fn ago_from(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..60 => "just now".into(),
        60..120 => "a minute ago".into(),
        120..3600 => format!("{} minutes ago", secs / 60),
        3600..7200 => "an hour ago".into(),
        7200..86_400 => format!("{} hours ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// The divider over a day's messages: today, yesterday, or the date.
fn day_label(at: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let now = now();
    let (Some(t), Some(today), Some(yesterday)) = (
        crate::platform::local_time(at),
        crate::platform::local_time(now),
        crate::platform::local_time(now.saturating_sub(86_400)),
    ) else {
        return String::new();
    };
    let day = (t.year, t.month, t.day);
    if day == (today.year, today.month, today.day) {
        "Today".into()
    } else if day == (yesterday.year, yesterday.month, yesterday.day) {
        "Yesterday".into()
    } else if t.year == today.year {
        format!("{} {}", MONTHS[t.month.clamp(0, 11) as usize], t.day)
    } else {
        format!(
            "{} {}, {}",
            MONTHS[t.month.clamp(0, 11) as usize],
            t.day,
            t.year
        )
    }
}

/// When something happened: the time today, else the day and the time.
fn when(at: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (Some(t), Some(today)) = (
        crate::platform::local_time(at),
        crate::platform::local_time(now()),
    ) else {
        return String::new();
    };
    let time = format!("{:02}:{:02}", t.hour, t.minute);
    if (t.year, t.month, t.day) == (today.year, today.month, today.day) {
        format!("today {time}")
    } else if t.year == today.year {
        format!("{} {} {time}", MONTHS[t.month.clamp(0, 11) as usize], t.day)
    } else {
        format!(
            "{} {} {}",
            MONTHS[t.month.clamp(0, 11) as usize],
            t.day,
            t.year
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_are_one_short_line() {
        assert_eq!(snippet("ship it"), "ship it");
        assert_eq!(snippet("first\nsecond"), "first …");
        let long = "x".repeat(200);
        assert_eq!(snippet(&long).chars().count(), SNIPPET + 1);
    }

    #[test]
    fn fingerprints_show_the_ends() {
        assert_eq!(fingerprint("5aGHOa1mWiRjRcHwVBGPDw"), "5aGH…GPDw");
        assert_eq!(fingerprint("ab"), "ab");
    }

    #[test]
    fn ago_reads_as_words() {
        assert_eq!(ago_from(1000, 1030), "just now");
        assert_eq!(ago_from(1000, 1000 + 4 * 60), "4 minutes ago");
        assert_eq!(ago_from(0, 3 * 3600), "3 hours ago");
        assert_eq!(ago_from(0, 100_000), "yesterday");
        assert_eq!(ago_from(0, 3 * 86_400), "3 days ago");
        assert_eq!(ago_from(50, 10), "just now");
    }

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(plural(1, "device", "devices"), "1 device");
        assert_eq!(plural(3, "person", "people"), "3 people");
    }
}
