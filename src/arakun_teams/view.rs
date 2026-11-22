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

use super::api::{DeviceRow, InviteRow, Role};
use super::hub::{self, ChannelState, Delivery, DeviceState, Hub, Load, Msg, Notice, TeamState};
use super::socket;
use super::{Access, Teams};
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
    /// A team this user asked to join, waiting for an admin.
    Asked(String),
    /// This user's own devices.
    Devices,
    /// The team's decision log.
    Decisions(String),
}

impl Pane {
    /// A team just made opens from the form that asked for it. Anywhere else
    /// the user moved on while it loaded: the pane stays, and what they
    /// typed there with it. The new team is in the team menu.
    fn opens_created(&self) -> bool {
        matches!(self, Pane::NewTeam | Pane::None)
    }

    /// Whether the bar over pages of no team offers "Start a team": an
    /// invite or a wait for an admin shows in place of the new-team form,
    /// and must not be the only thing a user without a team can do.
    /// `waiting` is an open invite or a request an admin has not answered.
    fn offers_start(&self, no_teams: bool, waiting: bool) -> bool {
        no_teams
            && match self {
                Pane::Invites | Pane::Asked(_) => true,
                Pane::None => waiting,
                _ => false,
            }
    }
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

#[derive(Default)]
struct Drafts {
    owner: Option<u64>,
    channels: HashMap<String, String>,
}

impl Drafts {
    /// Channel IDs are shared by teammates. A sign-out or direct switch
    /// discards drafts rather than letting the next member restore them.
    fn switch(&mut self, owner: Option<u64>) -> bool {
        if self.owner == owner {
            return false;
        }
        self.channels.clear();
        self.owner = owner;
        true
    }

    /// Keeps what is typed in the channel `pane` shows, if it shows one.
    fn keep(&mut self, pane: &Pane, typed: &str) {
        if let Pane::Channel(t, c) = pane
            && !typed.trim().is_empty()
        {
            self.channels
                .insert(hub::channel_key(t, c), typed.to_string());
        }
    }
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
    /// The team the field for a new channel is for: it shows in that team's
    /// channel list, and what is typed in it never goes to another team.
    adding_channel: Option<String>,
    /// "Create team" was pressed: the team the server names for it
    /// (`Hub::created`) opens once it arrives. Any other team arriving
    /// meanwhile, say from an approved invite, is not it.
    creating: bool,
    /// The invite accepted on the Invites page, as (team, invite): once the
    /// request is in, the sheet shows its wait instead of an empty list.
    accepting: Option<(String, String)>,
    team_name: Entity<TextInput>,
    invite: Entity<TextInput>,
    channel_name: Entity<TextInput>,
    composer: Entity<TextInput>,
    device_code: Entity<TextInput>,
    /// What was typed in other channels, by channel key.
    drafts: Drafts,
    /// The message the composer answers.
    reply_to: Option<String>,
    /// This user's message the composer edits.
    editing: Option<String>,
    /// The composer's text and reply from before the edit, put back when
    /// it ends so an unsent message is not lost.
    before_edit: Option<(String, Option<String>)>,
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
    /// The device being compared on Your devices, which has no side panel.
    comparing: Option<String>,
    copied: bool,
    /// Decided @bot answers opened again, by message ID: they fold once
    /// this user accepted them.
    unfolded: HashSet<String>,
    /// A message to scroll to once its channel shows it.
    pending_jump: Option<String>,
    /// The decision open in the Decisions panel, by its bot message ID.
    decision: Option<String>,
    /// Which decisions the log shows.
    decisions_filter: decisions_view::Filter,
    /// The decision whose "Reverse it" was pressed: its reason field shows.
    reversing: Option<String>,
    reverse_reason: Entity<TextInput>,
    /// The channels (team, channel) Decisions opened to read the log.
    decisions_opened: HashSet<(String, String)>,
    /// The decision log last built, and what it was built from.
    decision_log: Option<decisions_view::BuiltLog>,
    decision_query: Entity<TextInput>,
    /// The login typed to invite, as GitHub knows it.
    lookup: Option<(String, Lookup)>,
    _lookup: Option<Task<()>>,
    /// Whether the shell has this tab on screen, in a window in front. The
    /// view lives on behind another tab, and what arrives then is not read.
    shown: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

/// What GitHub said about a login typed to invite.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Lookup {
    Looking,
    /// The account, with its name when it has one.
    Found(Option<String>),
    Missing,
    /// GitHub could not say (a rate limit, no network).
    Unknown,
}

/// What the waiting page shows about one request to join.
struct Waiting {
    /// The team to remind, when the server knows the request.
    remind: Option<String>,
    name: String,
    subtitle: &'static str,
    approvers: Vec<String>,
    asked_at: Option<u64>,
    reminded_at: Option<u64>,
    remind_after: Option<u64>,
    reminding: bool,
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
        let reverse_reason = cx.new(|cx| TextInput::new("Why? (optional)", cx));
        let on = |cx: &mut Context<Self>,
                  input: &Entity<TextInput>,
                  act: fn(&mut Self, InputEvent, &mut Context<Self>)| {
            cx.subscribe(input, move |this, _, ev: &InputEvent, cx| {
                act(this, *ev, cx)
            })
        };
        let subs = vec![
            cx.observe(&teams, |this, _, cx| {
                this.sync_account(cx);
                cx.notify();
            }),
            cx.observe(&hub, |this, _, cx| this.hub_changed(cx)),
            on(cx, &team_name, |this, ev, cx| {
                if ev == InputEvent::Submit {
                    this.create_team(cx)
                }
            }),
            on(cx, &invite, |this, ev, cx| match ev {
                InputEvent::Submit => this.send_invite(cx),
                _ => this.look_up(cx),
            }),
            on(cx, &channel_name, |this, ev, cx| match ev {
                InputEvent::Submit => this.create_channel(cx),
                InputEvent::Cancel => {
                    this.adding_channel = None;
                    cx.notify();
                }
                // Typed on a team's page, which has the field without opening it.
                InputEvent::Changed
                    if this.adding_channel.is_none()
                        && !this.channel_name.read(cx).text().is_empty() =>
                {
                    this.adding_channel = this.selected_team().map(str::to_string);
                }
                _ => {}
            }),
            on(cx, &composer, |this, ev, cx| match ev {
                InputEvent::Submit => this.send(cx),
                InputEvent::Cancel => {
                    if this.editing.is_some() {
                        this.stop_editing(cx);
                    } else {
                        this.reply_to = None;
                    }
                    cx.notify();
                }
                // Up in an empty composer edits this user's last message.
                InputEvent::Up if this.composer.read(cx).text().is_empty() => {
                    if let Some(id) = this.last_own_text(cx) {
                        this.start_editing(id, cx);
                    }
                }
                _ => cx.notify(),
            }),
            on(cx, &device_code, |_, _, cx| cx.notify()),
            on(cx, &decision_query, |_, _, cx| cx.notify()),
            on(cx, &reverse_reason, |this, ev, cx| match ev {
                InputEvent::Submit => this.reverse_picked(cx),
                InputEvent::Cancel => {
                    this.reversing = None;
                    cx.notify();
                }
                _ => cx.notify(),
            }),
        ];
        let owner = Self::owner(teams.read(cx));
        Self {
            teams,
            hub,
            pane: Pane::None,
            side: None,
            team_menu: false,
            last_team: None,
            adding_channel: None,
            creating: false,
            accepting: None,
            team_name,
            invite,
            channel_name,
            composer,
            device_code,
            drafts: Drafts {
                owner,
                ..Default::default()
            },
            reply_to: None,
            editing: None,
            before_edit: None,
            scroll: ScrollHandle::new(),
            seen_count: 0,
            flash: None,
            _flash: None,
            confirm_remove: None,
            mismatch: None,
            comparing: None,
            copied: false,
            unfolded: HashSet::new(),
            pending_jump: None,
            decision: None,
            decision_log: None,
            decisions_filter: decisions_view::Filter::All,
            reversing: None,
            reverse_reason,
            decisions_opened: HashSet::new(),
            decision_query,
            lookup: None,
            _lookup: None,
            shown: false,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.hub.update(cx, |h, cx| h.refresh_unread(true, cx));
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The shell says whether this tab is on screen in a window in front.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            self.sync_viewing(cx);
        }
    }

    /// Tells the hub which channel shows, so what arrives there is read.
    fn sync_viewing(&self, cx: &mut Context<Self>) {
        let key = viewed(&self.pane, self.shown);
        self.hub.update(cx, |h, cx| h.set_viewing(key, cx));
    }

    /// The project @bot reads: the environment the window shows.
    pub fn set_project(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        self.hub
            .update(cx, |h, _| h.set_bot_project(Self::local_project(dir)));
    }

    /// A remote environment's path is an `arakun-server://` address, not a
    /// folder here: Claude Code could not start in it. @bot then runs as it
    /// does with no project open.
    fn local_project(dir: std::path::PathBuf) -> Option<std::path::PathBuf> {
        (!crate::model::Remote::is_path(&dir)).then_some(dir)
    }

    fn owner(teams: &Teams) -> Option<u64> {
        teams
            .me()
            .filter(|_| teams.enabled())
            .map(|me| me.github_id)
    }

    fn sync_account(&mut self, cx: &mut Context<Self>) {
        if !self.drafts.switch(Self::owner(self.teams.read(cx))) {
            return;
        }
        self.pane = Pane::None;
        self.reply_to = None;
        self.confirm_remove = None;
        self.comparing = None;
        self.flash = None;
        self._flash = None;
        self.copied = false;
        self.seen_count = 0;
        self.scroll = ScrollHandle::new();
        // What the last account searched for and had open in Decisions.
        self.decisions_opened.clear();
        self.decision = None;
        self.decisions_filter = decisions_view::Filter::All;
        self.reversing = None;
        for field in [
            &self.composer,
            &self.team_name,
            &self.channel_name,
            &self.invite,
            &self.device_code,
            &self.decision_query,
            &self.reverse_reason,
        ] {
            field.update(cx, |i, cx| i.set_text("", cx));
        }
    }

    /// `arakun peer-review <goal>` from an agent in this environment: runs the
    /// review this device paired in Teams and answers with its recommendation.
    pub fn peer_request(
        &self,
        project: Option<&std::path::Path>,
        args: &[String],
        done: futures::channel::oneshot::Sender<crate::control::Reply>,
        cx: &mut Context<Self>,
    ) {
        self.hub.update(cx, |h, cx| {
            let scope = h
                .peer_review
                .as_ref()
                .filter(|g| g.live())
                .and_then(|g| h.channels.get(&g.channel));
            let (Some(c), Some(project), [goal]) = (scope, project, args) else {
                let _ = done.send(crate::control::Reply::err(
                    "Pair one review in the Teams channel for this environment first.",
                ));
                return;
            };
            if Some(project) != h.bot.project.as_deref() {
                let _ = done.send(crate::control::Reply::err(
                    "This environment does not own the paired review.",
                ));
                return;
            }
            let (team, channel) = (c.team_id.clone(), c.channel_id.clone());
            match h.review_command(&team, &channel, &format!("review {goal}"), cx) {
                Ok(()) => {
                    h.peer_review
                        .as_mut()
                        .expect("request reserved a grant")
                        .caller = Some(done)
                }
                Err(e) => {
                    let _ = done.send(crate::control::Reply::err(e));
                }
            }
        });
    }

    /// Opens the first channel once there is one and nothing else was picked,
    /// opens a team just created, keeps the pane on things that still exist,
    /// and asks for the pictures of everyone the tab can show.
    fn hub_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_account(cx);
        let hub = self.hub.read(cx);
        if self.drafts.owner != hub.me().map(|me| me.github_id) {
            return;
        }
        let gone = match &self.pane {
            Pane::Channel(t, c) => !hub
                .team
                .get(t)
                .is_some_and(|s| s.channels.iter().any(|ch| ch.id == *c)),
            Pane::Team(t) | Pane::Invite(t) | Pane::Pending(t) | Pane::Decisions(t) => {
                !hub.teams.iter().any(|r| r.team_id == *t)
            }
            // Turned down, or let in (handled below).
            Pane::Asked(t) => !hub.asked().iter().any(|(a, _)| a == t),
            _ => false,
        };
        // The request went in: show its wait. It failed: the invite stays.
        let accepted = match &self.accepting {
            Some((t, _)) if hub.asked().iter().any(|(a, _)| a == t) => Some(Some(t.clone())),
            Some((_, invite)) if !hub.busy.contains(&format!("accept:{invite}")) => Some(None),
            _ => None,
        };
        // An admin let this user in while they looked at the wait. Not before
        // the team has loaded: until then it has no channel to open.
        let let_in = match &self.pane {
            Pane::Asked(t) if hub.team.contains_key(t) => Some(t.clone()),
            _ => None,
        };
        // A team still loading is not gone yet.
        let loading = match &self.pane {
            Pane::Channel(t, _) | Pane::Asked(t) => {
                hub.teams.iter().any(|r| r.team_id == *t) && !hub.team.contains_key(t)
            }
            _ => false,
        };
        let created = hub
            .created
            .clone()
            .filter(|t| self.creating && hub.team.contains_key(t));
        // Refused before the server made anything: the name stays to fix.
        let create_failed =
            self.creating && hub.created.is_none() && !hub.busy.contains("create-team");
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
        people.extend(
            hub.waiting
                .iter()
                .filter_map(|w| w.invited_by_login.clone()),
        );
        for t in hub.team.values() {
            people.extend(t.invites.iter().flatten().map(|i| i.login.clone()));
        }
        if let Some(accepted) = accepted {
            self.accepting = None;
            if let Some(t) = accepted
                && self.pane == Pane::Invites
            {
                self.pane = Pane::Asked(t);
            }
        }
        if let Some(t) = let_in {
            let first = self.first_channel(&t, cx);
            match first {
                Some((t, c)) => self.open_channel(t, c, cx),
                None => self.pane = Pane::Team(t),
            }
        } else if let Some(team) = created {
            self.creating = false;
            self.team_name.update(cx, |i, cx| i.set_text("", cx));
            if self.pane.opens_created() {
                self.pane = Pane::Team(team);
            }
        } else if create_failed {
            self.creating = false;
        } else if gone && !loading {
            self.keep_draft(cx);
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
            // Every team loaded and none has a channel (its setup failed
            // part way): its page, where an admin can make the first one.
            let channelless = hub
                .teams
                .iter()
                .all(|r| hub.team.contains_key(&r.team_id))
                .then(|| hub.teams.first().map(|r| r.team_id.clone()))
                .flatten();
            if let Some((t, c)) = first {
                self.open_channel(t, c, cx);
            } else if let Some(t) = channelless {
                self.pane = Pane::Team(t);
            }
        }
        // A channel made, or a membership read, while Decisions is open.
        if let Pane::Decisions(t) = &self.pane {
            let t = t.clone();
            self.open_every_channel(&t, true, cx);
        } else {
            self.close_decision_links(cx);
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
                // A refused channel has no link left: picking it again retries.
                self.hub
                    .update(cx, |h, cx| h.open_channel(&team, &channel, cx));
                return;
            }
        }
        self.keep_draft(cx);
        let name = self
            .hub
            .read(cx)
            .team
            .get(&team)
            .and_then(|t| t.channels.iter().find(|c| c.id == channel))
            .and_then(|c| c.name.clone())
            .unwrap_or_else(|| "this channel".into());
        let draft = self.drafts.channels.remove(&key).unwrap_or_default();
        self.composer.update(cx, |i, cx| {
            i.set_text(draft, cx);
            i.set_placeholder(format!("Message {name}"), cx);
        });
        self.reply_to = None;
        self.editing = None;
        self.before_edit = None;
        self.seen_count = 0;
        self.flash = None;
        self.hub
            .update(cx, |h, cx| h.open_channel(&team, &channel, cx));
        self.pane = Pane::Channel(team, channel);
        self.sync_viewing(cx);
        // A jump to a message (from Decisions) scrolls once the list is laid
        // out; until then the list scrolls to the bottom itself.
        if self.pending_jump.is_none() {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// Keeps what is typed for the channel in view. Opening a channel puts
    /// its draft in the composer, so call this wherever the pane leaves one.
    fn keep_draft(&mut self, cx: &gpui::App) {
        // An edit in progress is not a draft, but what it replaced is.
        let typed = match &self.before_edit {
            Some((typed, _)) if self.editing.is_some() => typed.clone(),
            _ => self.composer.read(cx).text().to_string(),
        };
        self.drafts.keep(&self.pane, &typed);
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
            self.open_every_channel(t, false, cx);
        }
        if let Pane::Channel(t, c) = pane {
            self.open_channel(t, c, cx);
        } else {
            self.keep_draft(cx);
            self.pane = pane;
            self.sync_viewing(cx);
        }
        // A new channel's name typed for one team does not follow to the next.
        if self.adding_channel.is_some() && self.adding_channel.as_deref() != self.selected_team() {
            self.adding_channel = None;
            self.channel_name.update(cx, |i, cx| i.set_text("", cx));
        }
        self.close_decision_links(cx);
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
        // Enter while a team is being made or opened would make a second one.
        if name.is_empty() || self.creating || self.hub.read(cx).busy.contains("create-team") {
            return;
        }
        // The name stays in the field until the team opens, to fix on a refusal.
        self.creating = true;
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
        self.lookup = None;
        self._lookup = None;
        self.hub.update(cx, |h, cx| h.invite(&team, login, cx));
    }

    /// Looks the typed login up on GitHub once typing pauses.
    fn look_up(&mut self, cx: &mut Context<Self>) {
        let login = self
            .invite
            .read(cx)
            .text()
            .trim()
            .trim_start_matches('@')
            .to_string();
        if self.lookup.as_ref().is_some_and(|(l, _)| *l == login) {
            return;
        }
        if login.is_empty() {
            self.lookup = None;
            self._lookup = None;
            cx.notify();
            return;
        }
        self.lookup = Some((login.clone(), Lookup::Looking));
        self._lookup = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let asked = login.clone();
            let found = cx
                .background_spawn(async move { super::api::github_user(&asked) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.lookup.as_ref().is_none_or(|(l, _)| *l != login) {
                    return;
                }
                let state = match found {
                    Ok(Some(user)) => {
                        let shown = user.login.clone();
                        this.teams.update(cx, |t, cx| t.want_pictures([shown], cx));
                        Lookup::Found(user.name.filter(|n| !n.trim().is_empty()))
                    }
                    Ok(None) => Lookup::Missing,
                    Err(_) => Lookup::Unknown,
                };
                this.lookup = Some((login, state));
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn create_channel(&mut self, cx: &mut Context<Self>) {
        let Some(team) = new_channel_team(self.adding_channel.as_deref(), self.selected_team())
        else {
            // The pane moved to another team under the field (its team went
            // away): the name was not typed for this one.
            self.adding_channel = None;
            self.channel_name.update(cx, |i, cx| i.set_text("", cx));
            cx.notify();
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
        // A refusal (the team waits for a new key) keeps the name to send later.
        if self.hub.read(cx).seal_blocked(&team).is_none() {
            self.channel_name.update(cx, |i, cx| i.set_text("", cx));
            self.adding_channel = None;
        }
        self.hub
            .update(cx, |h, cx| h.create_channel(&team, name, cx));
        cx.notify();
    }

    /// The open channel's messages, as held now.
    fn open_messages<'a>(&self, cx: &'a Context<Self>) -> Option<&'a [Msg]> {
        let Pane::Channel(t, c) = &self.pane else {
            return None;
        };
        let hub = self.hub.read(cx);
        Some(
            hub.channels
                .get(&hub::channel_key(t, c))?
                .messages
                .as_slice(),
        )
    }

    /// This user's newest text message in the open channel that was sent.
    fn last_own_text(&self, cx: &Context<Self>) -> Option<String> {
        let me = self.hub.read(cx).me()?.github_id;
        self.open_messages(cx)?
            .iter()
            .rev()
            .find(|m| m.github_id == me && m.body.kind == "text" && m.delivery == Delivery::Sent)
            .map(|m| m.id.clone())
    }

    /// Puts one of this user's messages in the composer to edit.
    fn start_editing(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(text) = self
            .open_messages(cx)
            .and_then(|ms| ms.iter().find(|m| m.id == id))
            .map(|m| m.body.text.clone())
        else {
            return;
        };
        // Switching to another edit keeps what the first one replaced.
        if self.editing.is_none() {
            let typed = self.composer.read(cx).text().to_string();
            self.before_edit = Some((typed, self.reply_to.take()));
        }
        self.editing = Some(id);
        self.reply_to = None;
        self.composer.update(cx, |i, cx| i.set_text(text, cx));
        cx.notify();
    }

    /// Ends an edit, sent or not, and puts back what the composer held.
    fn stop_editing(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        let (typed, reply_to) = self.before_edit.take().unwrap_or_default();
        self.reply_to = reply_to;
        self.composer.update(cx, |i, cx| i.set_text(typed, cx));
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let Pane::Channel(team, channel) = self.pane.clone() else {
            return;
        };
        let text = self.composer.read(cx).text().trim().to_string();
        // A refused socket does not retry, so a queued message would never go.
        let refused = self
            .hub
            .read(cx)
            .channels
            .get(&hub::channel_key(&team, &channel))
            .is_some_and(|c| matches!(c.status, socket::Status::Refused(_)));
        if text.is_empty() || refused {
            return;
        }
        if let Some(id) = self.editing.clone() {
            let unchanged = self
                .open_messages(cx)
                .and_then(|ms| ms.iter().find(|m| m.id == id))
                .is_some_and(|m| m.body.text == text);
            let sent = unchanged
                || self
                    .hub
                    .update(cx, |h, cx| h.edit(&team, &channel, &id, text, cx));
            if sent {
                self.stop_editing(cx);
            }
            return;
        }
        if text == "/agents" || text.starts_with("/agents ") {
            let result = self.hub.update(cx, |hub, cx| {
                hub.review_command(&team, &channel, text["/agents".len()..].trim(), cx)
            });
            match result {
                Ok(()) => self.composer.update(cx, |i, cx| i.set_text("", cx)),
                Err(why) => self.hub.update(cx, |h, cx| {
                    h.error = Some(why);
                    cx.notify();
                }),
            }
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
            // A long value wraps under itself instead of pushing the label out.
            .child(
                div()
                    .min_w_0()
                    .text_color(theme::text())
                    .text_right()
                    .child(value),
            )
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
        // An invite to another team expires unseen if only the team menu
        // lists it, so the bar shows it too.
        let invites = hub.invites.len();
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
                    .map(|d| ui::labelled(d, gpui::Role::Button, "Teams, devices, a new team"))
                    .map(ui::focusable)
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
                        ui::labelled(d, gpui::Role::Button, "Encryption")
                            .map(ui::focusable)
                            .cursor_pointer()
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
            .when(invites > 0, |d| {
                d.child(
                    ui::button_lead(
                        "teams-invites",
                        Some(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(theme::needs_you())
                                .into_any_element(),
                        ),
                        plural(invites, "invite", "invites"),
                        ButtonKind::Ghost,
                        true,
                    )
                    .tooltip(ui::tip("Invites to other teams"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.show(Pane::Invites, window, cx)),
                    ),
                )
            })
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
                        .map(|d| ui::labelled(d, gpui::Role::Button, "The team's people"))
                        .map(ui::focusable)
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

    /// The bar over pages of no team. Without a team there is no team menu,
    /// yet a second device needs Your devices on this one to be let in.
    fn render_teamless_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        // The new-team form opened from here has the same way back.
        let on_devices = matches!(self.pane, Pane::Devices | Pane::NewTeam);
        let hub = self.hub.read(cx);
        let waiting = !hub.invites.is_empty() || !hub.asked().is_empty();
        let start = self.pane.offers_start(hub.teams.is_empty(), waiting);
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .gap(ui::sp(4.))
            .h(px(ui::BAR_H))
            .px(ui::sp(12.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .when(start, |d| {
                d.child(
                    ui::button_with(
                        "teams-to-new",
                        Some("add01"),
                        "Start a team",
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.show(Pane::NewTeam, window, cx)),
                    ),
                )
            })
            .child(if on_devices {
                ui::button_with(
                    "teams-devices-back",
                    Some("arrow-left01"),
                    "Back",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| this.show(Pane::None, window, cx)))
            } else {
                ui::button_with(
                    "teams-to-devices",
                    Some("computer"),
                    "Your devices",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.hub.update(cx, |h, cx| h.load_my_devices(cx));
                    this.show(Pane::Devices, window, cx)
                }))
            })
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
        let asked = hub.asked();
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
        // Teams asked to join: their wait, until an admin lets this user in.
        for (i, (id, name)) in asked.into_iter().enumerate() {
            panel = panel.child(
                ui::menu_item(("teams-menu-asked", i), "clock01", name, false, true)
                    .when(self.pane == Pane::Asked(id.clone()), |d| {
                        d.bg(theme::selection())
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(Pane::Asked(id.clone()), window, cx)
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
        let creating = self.hub.read(cx).busy.contains("create-team") || self.creating;
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
        let invites: Vec<(InviteRow, bool, bool)> = hub
            .invites
            .iter()
            .map(|i| {
                let busy = hub.busy.contains(&format!("accept:{}", i.invite_id));
                let declining = hub.busy.contains(&format!("decline:{}", i.invite_id));
                (i.clone(), busy, declining)
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
        for (ix, (inv, busy, declining)) in invites.into_iter().enumerate() {
            let by = inv.invited_by_login.clone();
            let title = match &by {
                Some(by) => format!("{by} invited you to {}", inv.team_name),
                None => format!("You are invited to {}", inv.team_name),
            };
            let accept = inv.clone();
            let decline = inv.clone();
            let mut about = Vec::new();
            if let Some(n) = inv.member_count {
                about.push(format!("A team of {n}"));
            }
            about.push(format!("invited as {me}"));
            about.push(ago(inv.created_at));
            let mut about = about.join(" · ");
            if let Some(first) = about.get(..1) {
                about = first.to_uppercase() + &about[1..];
            }
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
                                                about,
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
                            div()
                                .flex()
                                .justify_end()
                                .gap(ui::sp(8.))
                                .child(
                                    ui::button(
                                        ("teams-decline", ix),
                                        if declining { "Declining…" } else { "Decline" },
                                        ButtonKind::Ghost,
                                        !busy && !declining,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.hub.update(cx, |h, cx| h.decline(&decline, cx));
                                        },
                                    )),
                                )
                                .child(
                                    ui::button(
                                        ("teams-accept", ix),
                                        if busy { "Joining…" } else { "Accept" },
                                        ButtonKind::Primary,
                                        !busy && !declining,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.accepting = Some((
                                                accept.team_id.clone(),
                                                accept.invite_id.clone(),
                                            ));
                                            this.hub.update(cx, |h, cx| h.accept(&accept, cx));
                                        },
                                    )),
                                ),
                        ),
                ),
            );
        }
        Self::scrolling("teams-invites-page", page)
    }

    /// An admin let this user in and the team is still loading, or failed
    /// to load. Not the wait: nothing is left for an admin to do.
    fn render_joining(&self, team: &str, failed: bool, cx: &mut Context<Self>) -> AnyElement {
        let subtitle = if failed {
            "An admin let you in, but the team did not load."
        } else {
            "An admin let you in. Loading the team…"
        };
        let mut page = Self::page().child(ui::page_title(
            team.to_string(),
            Some(subtitle.into_any_element()),
        ));
        if failed {
            page = page.child(
                div().flex().child(
                    ui::button(
                        "teams-joining-retry",
                        "Try again",
                        ButtonKind::Primary,
                        true,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.hub.update(cx, |h, cx| h.retry(cx))),
                    ),
                ),
            );
        }
        Self::scrolling("teams-joining-page", page)
    }

    /// "Waiting for an admin to approve this device": after accepting an
    /// invite, or a device of a member that holds no team key yet.
    fn render_waiting(&self, w: Waiting, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let device = crate::redact::value(&hub::device_name());
        let admins = w.approvers;
        let asked = match w.asked_at {
            Some(at) => format!("{device} · asked {}", ago(at)),
            None => format!("{device} · asked to join"),
        };
        let wait_until = remind_wait(w.remind_after, w.reminded_at, now());
        let remind = w.remind.map(|team| {
            let who = admins
                .first()
                .cloned()
                .unwrap_or_else(|| "the admins".into());
            let label = if w.reminding {
                "Reminding…".to_string()
            } else {
                format!("Remind {who}")
            };
            ui::button(
                "teams-remind",
                label,
                ButtonKind::Outline,
                !w.reminding && wait_until.is_none(),
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.hub.update(cx, |h, cx| h.remind(&team, cx))),
            )
        });
        let remind_note = match (wait_until, w.reminded_at) {
            (Some(at), Some(sent)) => Some(format!(
                "Reminded {}. You can remind them again {}.",
                ago(sent),
                until(at)
            )),
            (Some(at), None) => Some(format!("You can remind them again {}.", until(at))),
            (None, Some(at)) => Some(format!("Reminded {}.", ago(at))),
            _ => None,
        };
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
                    w.name.clone(),
                    Some(w.subtitle.into_any_element()),
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
                                    .child(Self::tile(ui::spinner(16., theme::accent())))
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
                                                ui::text(Type::Label, theme::text_subtle(), asked)
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
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(ui::sp(12.))
                                    .child(
                                        Self::note(
                                            "square-lock02",
                                            "An admin's device holds the key. The server does not.",
                                        )
                                        .flex_1()
                                        .min_w_0(),
                                    )
                                    .children(remind),
                            )
                            .children(remind_note.map(Self::footnote)),
                    ),
                ),
        )
    }

    // ---- a team's pages ------------------------------------------------------------

    /// The team's people, with "Invite people", and a way to the approvals.
    fn render_team(&self, team_id: &str, faces: &Faces, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return ui::loading("Reading the team…").into_any_element();
        };
        let me = hub.me().map_or(0, |m| m.github_id);
        let my_login = hub.me().map(|m| m.login.clone());
        let admin = t.role.is_admin();
        let role = t.role;
        let members = t.detail.members.clone();
        let devices: HashMap<u64, Vec<DeviceRow>> = t.devices.clone();
        let pending = t.pending.len();
        let problem = t.problem.clone();
        let name = t.detail.name.clone();
        let first = t.channels.first().map(|c| (c.id.clone(), c.name.clone()));
        let can_add_channel = admin && !t.keyless;
        let making = hub.busy.contains(&format!("channel:{team_id}"));
        let can_seal = hub.seal_blocked(team_id).is_none();
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
                    shown_login(&m.login, my_login.as_deref()),
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
        } else if can_add_channel {
            // A team whose first channel was not made: there is no channel
            // column to add one from, so it starts here.
            page = page.child(
                Self::card().child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .p(ui::sp(12.))
                        .child(
                            ui::field()
                                .flex_1()
                                .child(icon("comment01", 14.).text_color(theme::text_subtle()))
                                .child(div().flex_1().child(self.channel_name.clone())),
                        )
                        .child(
                            ui::button(
                                "teams-first-channel",
                                if making {
                                    "Making…"
                                } else {
                                    "Make the first channel"
                                },
                                ButtonKind::Primary,
                                !making && can_seal,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.create_channel(cx))),
                        ),
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
        let sent: Vec<(String, String, u64, bool)> = t
            .and_then(|t| t.invites.clone())
            .unwrap_or_default()
            .into_iter()
            .rev()
            .filter(|i| !members.contains(&i.login.to_ascii_lowercase()))
            .map(|i| {
                let busy = hub.busy.contains(&format!("withdraw:{}", i.invite_id));
                (i.invite_id, i.login, i.created_at, busy)
            })
            .collect();
        let tid = team_id.to_string();
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
        let found = match self.lookup.as_ref().filter(|(l, _)| *l == typed) {
            Some((_, Lookup::Found(Some(name)))) => format!("{name} · found on GitHub"),
            Some((_, Lookup::Found(None))) => "Found on GitHub".to_string(),
            Some((_, Lookup::Missing)) => "Not found on GitHub".to_string(),
            Some((_, Lookup::Looking)) => "Looking on GitHub…".to_string(),
            _ => "A GitHub login".to_string(),
        };
        if !typed.is_empty() {
            card = card.child(
                div().px(ui::sp(CARD_INSET)).pb(ui::sp(CARD_INSET)).child(
                    Self::person_row(faces.avatar(&typed, AVATAR), typed.clone(), found, true)
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
            for (i, (invite_id, login, at, busy)) in sent.into_iter().enumerate() {
                let note = if pending.contains(&login.to_ascii_lowercase()) {
                    format!("Invited {} · accepted, waiting for approval", ago(at))
                } else {
                    format!("Invited {} · has not answered", ago(at))
                };
                let t = tid.clone();
                rows = rows.child(
                    Self::person_row(faces.avatar(&login, AVATAR), login, note, false).child(
                        ui::button(
                            ("teams-withdraw", i),
                            if busy { "Withdrawing…" } else { "Withdraw" },
                            ButtonKind::Ghost,
                            !busy,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hub.update(cx, |h, cx| h.withdraw(&t, &invite_id, cx))
                        })),
                    ),
                );
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
            return ui::loading("Reading the team…").into_any_element();
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
            let request = p.request_id.clone();
            let reject_request = request.clone();
            let busy = approving[i];
            rows = rows
                .child(
                    Self::person_row(
                        faces.avatar(&p.login, AVATAR),
                        p.login.clone(),
                        match p.reminded_at {
                            Some(at) => format!(
                                "Accepted {} · {} · reminded you {}",
                                ago(p.requested_at),
                                plural(p.devices.len(), "device", "devices"),
                                ago(at)
                            ),
                            None => format!(
                                "Accepted {} · {}",
                                ago(p.requested_at),
                                plural(p.devices.len(), "device", "devices")
                            ),
                        },
                        false,
                    )
                    .child(
                        ui::button(("teams-reject", i), "Reject", ButtonKind::Ghost, !busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.hub.update(cx, |h, cx| {
                                    h.reject(&t1, gid, reject_request.clone(), cx)
                                })
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
                            this.hub
                                .update(cx, |h, cx| h.approve(&t2, gid, &request, cx))
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
        for (i, d) in devices.iter().enumerate() {
            let this_one = d.device_id == mine;
            let id = d.device_id.clone();
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
                })
                .when(hub.mismatched(&d.device_id), |r| {
                    r.child(ui::badge("Does not match", Tone::Danger))
                })
                .when(hub.verified(&d.device_id), |r| {
                    r.child(ui::badge("Checked", Tone::Success))
                })
                // A device marked as not matching blocks a new team's keys,
                // and without a team there is no channel to compare it from.
                .when(!this_one, |r| {
                    r.child(
                        ui::button(
                            ("teams-device-compare", i),
                            "Compare",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.comparing = Some(id.clone());
                            this.mismatch = None;
                            cx.notify();
                        })),
                    )
                }),
            );
        }
        let my_login = hub.me().map(|m| m.login.clone()).unwrap_or_default();
        let compare = self
            .comparing
            .as_ref()
            .and_then(|id| devices.iter().find(|d| d.device_id == *id && *id != mine))
            .map(|d| self.render_compare_device(&my_login, d, cx))
            .map(|(title, body)| {
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
                                .justify_between()
                                .gap(ui::sp(8.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .ty(Type::Body)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(title),
                                )
                                .child(
                                    ui::icon_button(
                                        "teams-device-compare-close",
                                        "cancel01",
                                        14.,
                                        "Close",
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.comparing = None;
                                            this.mismatch = None;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                        .child(body),
                )
            });
        let page = Self::page()
            .child(ui::page_title(
                "Your devices",
                Some(
                    "Every device signs what it sends, and holds its own copy of each team key."
                        .into_any_element(),
                ),
            ))
            .child(Self::card().child(rows))
            .children(compare)
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
        let my_login = hub.me().map(|m| m.login.as_str());
        let people: Vec<(String, String, bool)> = hub::here_now(&t.detail.members, here)
            .into_iter()
            .map(|(m, on)| (m.login.clone(), shown_login(&m.login, my_login), on))
            .collect();
        let making = hub.busy.contains(&format!("channel:{team_id}"));
        let tid = team_id.to_string();
        let add_to = team_id.to_string();
        let mut list =
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(2.))
                .child(Self::column_title("Channels").when(admin, |d| {
                    d.child(
                        ui::icon_button("teams-add-channel", "add01", 12., "New channel").on_click(
                            cx.listener(move |this, _, window, cx| {
                                let open = this.adding_channel.as_deref() == Some(&*add_to);
                                this.adding_channel = (!open).then(|| add_to.clone());
                                if !open {
                                    this.channel_name.update(cx, |i, cx| i.set_text("", cx));
                                    let handle = this.channel_name.read(cx).focus_handle(cx);
                                    window.focus(&handle, cx);
                                }
                                cx.notify();
                            }),
                        ),
                    )
                }));
        for (i, (cid, cname, unread)) in channels.into_iter().enumerate() {
            let on = cid == channel_id;
            let (t2, c2) = (tid.clone(), cid.clone());
            let cname = cname.unwrap_or_else(|| "locked channel".into());
            list = list.child(
                div()
                    .id(("teams-channel", i))
                    .map(|d| ui::labelled(d, gpui::Role::Button, format!("Open {cname}")))
                    .map(ui::focusable)
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
                    .child(div().flex_1().min_w_0().truncate().child(cname))
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
        if self.adding_channel.as_deref() == Some(team_id) || making {
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
                        ui::spinner(12., theme::text_faint()).into_any_element()
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
        for (login, shown, on) in people {
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
                    .child(div().flex_1().min_w_0().truncate().child(shown))
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

    // A row needs the message, its place and its quote, and what every row
    // shares: a struct for them would be built once per row and read once.
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
        let my_login = self.hub.read(cx).me().map(|m| m.login.clone());
        let lit = !mark.in_range
            && (self.flash.as_deref() == Some(m.id.as_str())
                || self.reply_to.as_deref() == Some(m.id.as_str()));
        let quote = m.body.reply_to.as_ref().map(|target| match quoted {
            Some((qi, q)) => {
                let qid = q.id.clone();
                div()
                    .id(("teams-quote", index))
                    .map(|d| ui::labelled(d, gpui::Role::Button, "Jump to the message"))
                    .map(ui::focusable)
                    .flex()
                    .items_center()
                    .gap(ui::sp(6.))
                    .min_w_0()
                    .pb(ui::sp(2.))
                    .cursor_pointer()
                    .child(icon("arrow-turn-forward", 12.).text_color(theme::text_faint()))
                    .child(
                        ui::text(
                            Type::Caption,
                            theme::text_subtle(),
                            shown_login(&q.login, my_login.as_deref()),
                        )
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
                            .child(snippet(&crate::redact::text(&q.body.text))),
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
            "decision_reverse" => Some("reversed"),
            "agent_review" => {
                Some(super::coord::Event::read(&m.body).map_or("Agent review", |e| e.label()))
            }
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
                            ui::text(
                                Type::Body,
                                theme::content(),
                                shown_login(&m.login, my_login.as_deref()),
                            )
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
                    .child(edited_text(
                        &crate::redact::text(&m.body.text),
                        m.edited.is_some(),
                    )),
            )
            .children(delivery);
        let editable = m.body.kind == "text"
            && m.delivery == Delivery::Sent
            && self
                .hub
                .read(cx)
                .me()
                .is_some_and(|me| me.github_id == m.github_id);
        let edit_id = m.id.clone();
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
                        .flex()
                        .when(editable, |d| {
                            d.child(
                                ui::icon_button(
                                    ("teams-edit", index),
                                    "pencil-edit01",
                                    13.,
                                    "Edit (Up in an empty message)",
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.start_editing(edit_id.clone(), cx);
                                        let handle = this.composer.read(cx).focus_handle(cx);
                                        window.focus(&handle, cx);
                                    },
                                )),
                            )
                        })
                        .child(
                            ui::icon_button(
                                ("teams-reply", index),
                                "arrow-turn-backward",
                                13.,
                                "Reply",
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    if this.editing.is_some() {
                                        this.stop_editing(cx);
                                    }
                                    this.reply_to = Some(id.clone());
                                    let handle = this.composer.read(cx).focus_handle(cx);
                                    window.focus(&handle, cx);
                                    cx.notify();
                                },
                            )),
                        ),
                )
            })
            .into_any_element()
    }

    /// "sending…", or why it was not sent and a way to send it again. For
    /// a sent message, the same for its newest edit, whose own row is hidden.
    fn delivery(
        &self,
        index: usize,
        m: &Msg,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (delivery, mid, is_edit) = match &m.delivery {
            Delivery::Sent => {
                let e = self.open_messages(cx).and_then(|ms| last_edit(ms, &m.id))?;
                (e.delivery.clone(), e.id.clone(), true)
            }
            other => (other.clone(), m.id.clone(), false),
        };
        match delivery {
            Delivery::Sent => None,
            Delivery::Sending => {
                Some(ui::text(Type::Caption, theme::text_faint(), "sending…").into_any_element())
            }
            Delivery::Failed(why) => {
                let (t, c) = (team_channel.0.to_string(), team_channel.1.to_string());
                let why = if is_edit {
                    format!("edit not sent: {why}")
                } else {
                    format!("not sent: {why}")
                };
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(6.))
                        .child(ui::text(Type::Caption, theme::error().into(), why))
                        .child(
                            div()
                                .id(("teams-resend", index))
                                .map(|d| ui::labelled(d, gpui::Role::Button, "Send again"))
                                .map(ui::focusable)
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
        let dismiss_id = n.device_id.clone();
        let hub = self.hub.read(cx);
        let verified = hub.verified(&n.device_id);
        let login = shown_login(&n.login, hub.me().map(|m| m.login.as_str()));
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
                    .child(format!("{login} added a new device,\u{a0}"))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(crate::redact::text(&n.device).into_owned()),
                    )
                    .child("."),
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
            // Comparing may have to wait, or end in "They don't": the notice
            // must not stay in every channel of the team for the whole run.
            .child(
                ui::icon_button(("teams-notice-dismiss", i), "cancel01", 12., "Dismiss").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.hub
                            .update(cx, |h, cx| h.dismiss_notice(&dismiss_id, cx))
                    }),
                ),
            )
            .into_any_element()
    }

    fn render_composer(
        &self,
        can_send: bool,
        keyless: bool,
        blocked: Option<String>,
        connected: bool,
        bot_hint: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let my_login = self.hub.read(cx).me().map(|m| m.login.clone());
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
                                .child(shown_login(&r.login, my_login.as_deref())),
                        )
                        .child(div().min_w_0().truncate().child(format!(
                            ":\u{a0}{}",
                            snippet(&crate::redact::text(&r.body.text))
                        ))),
                )
                .child(
                    ui::icon_button("teams-reply-cancel", "cancel01", 12., "Cancel reply (Esc)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reply_to = None;
                            cx.notify();
                        })),
                )
        });
        let edit_bar = self.editing.as_ref().map(|_| {
            Self::above_row()
                .child(icon("pencil-edit01", 12.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child("Editing your message"),
                )
                .child(
                    ui::icon_button("teams-edit-cancel", "cancel01", 12., "Cancel edit (Esc)")
                        .on_click(cx.listener(|this, _, _, cx| this.stop_editing(cx))),
                )
        });
        let reply_bar = edit_bar.or(reply_bar);
        // An edit never runs @bot, so the hint would mislead.
        let bot_hint = bot_hint.filter(|_| self.editing.is_none());
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
                                    .map(|d| ui::labelled(d, gpui::Role::Button, "Send (Enter)"))
                                    .when(ready, ui::focusable)
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
                        match blocked {
                            _ if keyless => {
                                "You can write here once this device holds the team key.".into()
                            }
                            Some(why) => why,
                            None if can_send => {
                                "Not connected right now. What you send goes out once it is back."
                                    .into()
                            }
                            None => "The channel turned this device away. Open it again to retry."
                                .into(),
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
            return ui::loading("Reading the team…").into_any_element();
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
        let peer_hint = hub
            .peer_review
            .as_ref()
            .filter(|g| g.channel == key && g.live())
            .map(|g| {
                format!(
                    "Paired with @{} · /agents review <goal> · /agents off",
                    t.login(g.peer)
                )
            })
            .unwrap_or_else(|| {
                "Pair agents: /agents pair @login · one Claude turn, shared text only ($0.10 limit)"
                    .into()
            });
        let loading = c.is_none_or(|c| c.loading);
        let (hidden, locked) = c.map_or((0, 0), |c| (c.hidden, c.locked()));
        let keyless = t.keyless;
        let joined = hub.joined.contains(team_id);
        let team_name = t.detail.name.clone();
        let blocked = hub.seal_blocked(team_id);
        // It can be this user, from another device: masked like any login.
        let approved_by = t
            .approved_by
            .as_deref()
            .map(|l| shown_login(l, hub.me().map(|m| m.login.as_str())));
        // Each notice goes where its device was added among the messages.
        let added: HashMap<String, u64> = t
            .devices
            .values()
            .flatten()
            .map(|d| (d.device_id.clone(), d.created_at))
            .collect();
        let notices = notices_in_order(&hub.notices, team_id, &added);

        let mut bot = self.bot_state(&messages, team_id, channel_id, cx);

        // The list, laid out first so a quote knows its target's row.
        let mut lines: Vec<Line> = Vec::new();
        let mut day = None;
        let mut placed = vec![false; notices.len()];
        let mut runs_placed = vec![false; bot.runs.len()];
        for (i, m) in messages.iter().enumerate() {
            // Acceptances, reversals and edits show as the line under what
            // they change. A refused acceptance or reversal stays a row, so
            // its error and Send again show.
            if accept_shown_under(m, &messages, &bot.decided) || hidden_row(m) {
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
            if bot.decided.contains_key(&m.id) || bot.reversed.contains_key(&m.id) {
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
        let jump = self
            .pending_jump
            .clone()
            .and_then(|id| Some((*index_of.get(id.as_str())?, id)));

        // Keep the newest in view when it arrives and the list was at its
        // bottom. A run of @bot counts, and so does its answer replacing it.
        // Not when this frame jumps to a message: the scroll to the bottom
        // is applied after the one to the message, and would win.
        let seen = messages.len() * 2 + bot.runs.len();
        if seen != self.seen_count {
            let max = self.scroll.max_offset().y;
            let at_bottom = -self.scroll.offset().y >= max - px(48.);
            if (self.seen_count == 0 || at_bottom) && jump.is_none() {
                self.scroll.scroll_to_bottom();
            }
            self.seen_count = seen;
        }
        if let Some((row, id)) = jump {
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
            ui::loading("Reading the channel…").into_any_element()
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
                        "{} approved {}. You can read {team_name} from here on, and its history.",
                        approved_by.as_deref().unwrap_or("An admin"),
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

        // A refused socket does not retry, so nothing queued would go out.
        let refused = matches!(status, socket::Status::Refused(_));
        let can_send = !keyless && !refused && blocked.is_none();
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
                status_line.filter(|_| refused || !messages.is_empty()),
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
            .child(div().px(ui::sp(16.)).child(ui::text(
                Type::Caption,
                theme::text_subtle(),
                peer_hint,
            )))
            .child(self.render_composer(
                can_send,
                keyless,
                blocked,
                connected,
                bot.hint.take(),
                cx,
            ));

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
                        ui::icon_button("teams-side-close", "cancel01", 14., "Close").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.side = None;
                                this.mismatch = None;
                                cx.notify();
                            }),
                        ),
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
        let epoch = match t.epoch_made.get(&t.detail.epoch) {
            Some(at) => format!("Epoch {} · made {}", t.detail.epoch, made_on(*at)),
            None => format!("Epoch {}", t.detail.epoch),
        };
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
                    .child(Self::fact("Team key", epoch))
                    .child(Self::fact(
                        "Held by",
                        format!(
                            "{} · {}",
                            plural(people, "person", "people"),
                            plural(devices, "device", "devices")
                        ),
                    ))
                    // As "What the server sees" in teams/PROTOCOL.md has it.
                    .child(Self::fact(
                        "The server sees",
                        "Team and device names, GitHub logins, who is in the team, and when",
                    ))
                    .child(Self::fact("The server can't see", "Messages and channel names")),
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
        let my_login = hub.me().map(|m| m.login.clone());
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
            let label = format!("Compare {}", crate::redact::text(&d.name));
            list = list.child(
                div()
                    .id(("teams-pick-device", i))
                    .map(|d| ui::labelled(d, gpui::Role::Button, label))
                    .map(ui::focusable)
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
                            .child(format!(
                                "{} · {}",
                                shown_login(&login, my_login.as_deref()),
                                crate::redact::text(&d.name)
                            )),
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
        self.render_compare_device(&login, &device, cx)
    }

    /// The compare numbers for a device, found in a team or, on Your
    /// devices, among this user's own.
    fn render_compare_device(
        &self,
        login: &str,
        device: &DeviceRow,
        cx: &mut Context<Self>,
    ) -> (String, AnyElement) {
        let hub = self.hub.read(cx);
        let device_id = device.device_id.as_str();
        let me = hub.me().map(|m| m.login.as_str());
        // Your own device has no one else to ask.
        let own = me.is_some_and(|me| me.eq_ignore_ascii_case(login));
        // One of your own devices shows your login: mask it in redacted mode.
        let login = shown_login(login, me);
        let name = crate::redact::text(&device.name).into_owned();
        let numbers = hub.public_keys().and_then(|mine| {
            super::crypto::compare_numbers(mine, (&device.sig_pub, &device.enc_pub))
        });
        let verified = hub.verified(device_id);
        let mismatch = self.mismatch.as_deref() == Some(device_id) || hub.mismatched(device_id);
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
                if own {
                    format!(
                        "On {name}, open Your devices and press Compare for this device. If both show the same numbers, it is yours."
                    )
                } else {
                    format!(
                        "Ask {login} to open Compare on {name}. If both show the same numbers, it is theirs."
                    )
                },
            ))
            .child(grid)
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
                                .child(if own {
                                    format!("You checked {name}. It is yours.")
                                } else {
                                    format!("You checked {name} with {login}. It is theirs.")
                                }),
                        )
                        .into_any_element(),
                )
            } else if mismatch {
                Some(

                ui::text(
                    Type::Label,
                    theme::error().into(),
                    if own {
                        format!(
                            "The numbers differ: {name} may not be yours. It gets no team keys from this device until they match."
                        )
                    } else {
                        format!(
                            "The numbers differ: {name} may not be {login}'s. Tell {login} and a team admin, and read its messages with care."
                        )
                    },
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
                            // The notice stays to show Checked; it has its own Dismiss.
                            this.hub.update(cx, |h, cx| h.set_verified(&id, true, cx));
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
                                            .child(crate::redact::value(code)),
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
                                    .child(ui::spinner(14., theme::text_subtle()))
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
        // A device without the team key waits instead of a channel it can't
        // read. The team's pages stay: rejecting a request and leaving need
        // no key, and Approve is off there until the key comes.
        let waiting_on = match &self.pane {
            Pane::Channel(t, _) => Some(t.as_str()),
            _ => None,
        }
        .filter(|t| keyless(t))
        .map(|t| (t.to_string(), name_of(t), admins_of(t)));
        let inviters = hub.inviters.clone();
        let waiting_of = |t: &str, name: String, subtitle: &'static str, admins: Vec<String>| {
            let row = hub.waiting_for(t);
            // The one who invited can approve; name them first.
            let mut approvers = admins;
            if let Some(by) = row
                .and_then(|w| w.invited_by_login.clone())
                .or_else(|| inviters.get(t).cloned())
            {
                approvers.retain(|a| !a.eq_ignore_ascii_case(&by));
                approvers.insert(0, by);
            }
            Waiting {
                remind: row.map(|w| w.team_id.clone()),
                name,
                subtitle,
                approvers,
                asked_at: row.map(|w| w.requested_at),
                reminded_at: row.and_then(|w| w.reminded_at),
                remind_after: hub.remind_after.get(t).copied(),
                reminding: hub.busy.contains(&format!("remind:{t}")),
            }
        };
        let waiting_page = if let Some((t, name, admins)) = waiting_on.clone() {
            Some(waiting_of(
                &t,
                name,
                "This device is new to the team. One step is left, and it is not yours.",
                admins,
            ))
        } else if let Pane::Asked(t) = &self.pane {
            let name = asked
                .iter()
                .find(|(a, _)| a == t)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            Some(waiting_of(
                t,
                name,
                "You accepted the invite. One step is left, and it is not yours.",
                Vec::new(),
            ))
        } else if no_teams && !has_invites && self.pane == Pane::None && !asked.is_empty() {
            let (t, name) = asked[0].clone();
            Some(waiting_of(
                &t,
                name,
                "You accepted the invite. One step is left, and it is not yours.",
                Vec::new(),
            ))
        } else {
            None
        };
        let first_team = hub.teams.first().map(|r| r.team_id.clone());
        let back_team = || {
            self.last_team
                .clone()
                .filter(|t| !name_of(t).is_empty())
                .or_else(|| first_team.clone())
        };

        let (header_team, header_channel, body): (Option<String>, Option<String>, AnyElement) =
            match self.pane.clone() {
                _ if waiting_on.is_some() => {
                    let (t, _, _) = waiting_on.expect("checked");
                    let w = waiting_page.expect("made with waiting_on");
                    (Some(t), None, self.render_waiting(w, faces, cx))
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
                // Not a channel: no channel name, no Encryption panel to
                // open, and the Chat button leads back.
                Pane::Decisions(t) => (Some(t.clone()), None, self.render_decisions(&t, faces, cx)),
                Pane::Devices => (back_team(), None, self.render_devices(cx)),
                // The header's menu is the way back to the team.
                Pane::NewTeam => (back_team(), None, self.render_new_team(cx)),
                Pane::Invites => (back_team(), None, self.render_invites(faces, cx)),
                // Let in, with the team not loaded yet: the request is gone,
                // so its name comes from the team list.
                Pane::Asked(t)
                    if !asked.iter().any(|(a, _)| *a == t) && !name_of(&t).is_empty() =>
                {
                    let name = name_of(&t);
                    let failed = unreachable.is_some();
                    (back_team(), None, self.render_joining(&name, failed, cx))
                }
                Pane::Asked(_) => {
                    let w = waiting_page.expect("made for Pane::Asked");
                    (back_team(), None, self.render_waiting(w, faces, cx))
                }
                Pane::None if no_teams && has_invites => {
                    (None, None, self.render_invites(faces, cx))
                }
                Pane::None if no_teams && !asked.is_empty() => {
                    let w = waiting_page.expect("made for this case");
                    (None, None, self.render_waiting(w, faces, cx))
                }
                Pane::None if no_teams => (None, None, self.render_new_team(cx)),
                Pane::None => (None, None, Self::centered(ui::loading("Opening the team…"))),
            };
        let header = match header_team {
            Some(t) => self.render_header(&t, header_channel, faces, cx),
            None => self.render_teamless_bar(cx),
        };
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
            .child(header)
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
                    ui::icon_button("teams-info-dismiss", "cancel01", 12., "Dismiss").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.hub.update(cx, |h, cx| {
                                h.info = None;
                                cx.notify();
                            })
                        }),
                    ),
                )
            }))
            .children(error.map(|e| {
                Self::banner(
                    "alert-circle",
                    Tone::Warning,
                    crate::redact::text(&e).into_owned(),
                )
                .child(
                    ui::icon_button("teams-error-dismiss", "cancel01", 12., "Dismiss").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.hub.update(cx, |h, cx| {
                                h.error = None;
                                cx.notify();
                            })
                        }),
                    ),
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
        let access = teams.access();
        let account_error = teams.error().map(|e| e.to_string());
        let faces = Faces::of(teams);
        let hub = self.hub.read(cx);
        // Notifications can arrive in either order. Never draw one member's
        // messages or fields under another member's login.
        if self.drafts.owner != Self::owner(teams)
            || self.drafts.owner != hub.me().map(|me| me.github_id)
        {
            return Self::centered(ui::loading("Loading account…"));
        }
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

        let body: AnyElement = if access == Access::Unknown {
            Self::centered(ui::empty_state(
                "user",
                "Checking your account",
                account_error.as_deref().unwrap_or(
                    "The Teams server has not said yet whether Teams is on for this account.",
                ),
                None,
            ))
        } else if access == Access::Off {
            Self::centered(ui::empty_state(
                "user",
                "Team is not on for this account",
                "The Teams server has not turned Team on for the account you signed in with.",
                None,
            ))
        } else if !enabled {
            // The tab can stay open after a sign-out, or after the server took
            // the flag away. Only a signed-out account is asked to sign in.
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
                    Self::centered(ui::loading("Setting up this device…"))
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
                (_, Load::Loading) if empty => Self::centered(ui::loading("Reading your teams…")),
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
                        // The click that closes the menu must not also press
                        // Remove, Approve or a channel row under it.
                        .occlude()
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
            .into_any_element()
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

/// A team's device notices with when each device was added, oldest first.
/// The hub lists them as it walks the roster, member by member, so two
/// devices added between the same messages would show in that order. A
/// device the roster no longer has goes last.
fn notices_in_order(
    notices: &[Notice],
    team_id: &str,
    added: &HashMap<String, u64>,
) -> Vec<(Notice, u64)> {
    let mut notices: Vec<(Notice, u64)> = notices
        .iter()
        .filter(|n| n.team_id == team_id)
        .map(|n| {
            (
                n.clone(),
                added.get(&n.device_id).copied().unwrap_or(u64::MAX),
            )
        })
        .collect();
    notices.sort_by_key(|(_, at)| *at);
    notices
}

/// The team a new channel goes to: the one on show, when the field was
/// opened or typed in for it. A name typed for another team makes nothing.
fn new_channel_team(field: Option<&str>, shown: Option<&str>) -> Option<String> {
    shown.filter(|t| field == Some(*t)).map(str::to_string)
}

/// A login as the sheet shows it. The signed-in user's own one is masked in
/// redacted mode, as on the Invites page: next to "You" it tells a screen
/// share whose account this is. Other people's logins stay, the team is them.
fn shown_login(login: &str, me: Option<&str>) -> String {
    if me.is_some_and(|me| me.eq_ignore_ascii_case(login)) {
        crate::redact::value(login)
    } else {
        login.to_string()
    }
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

/// The channel the user reads now: the pane's, while the tab is on screen.
fn viewed(pane: &Pane, shown: bool) -> Option<String> {
    match pane {
        Pane::Channel(t, c) if shown => Some(hub::channel_key(t, c)),
        _ => None,
    }
}

/// How long the server takes no second reminder from the same person
/// (`REMIND_EVERY` in teams/src/teams.ts).
const REMIND_EVERY: u64 = 60 * 60;

/// Until when Remind waits, if it still does: the time the server gave when
/// it refused one, or an hour after the last one it took, whichever is later.
fn remind_wait(remind_after: Option<u64>, reminded_at: Option<u64>, now: u64) -> Option<u64> {
    remind_after
        .max(reminded_at.map(|at| at.saturating_add(REMIND_EVERY)))
        .filter(|at| *at > now)
}

/// Whether a reversal or an edit has no row of its own: an edit never has,
/// a reversal unless it was refused. [`accept_shown_under`] decides for an
/// acceptance.
fn hidden_row(m: &Msg) -> bool {
    match m.body.kind.as_str() {
        "decision_reverse" => !matches!(m.delivery, Delivery::Failed(_)),
        "edit" => true,
        _ => false,
    }
}

/// The newest edit of a message, in the order the channel holds them.
fn last_edit<'a>(messages: &'a [Msg], id: &str) -> Option<&'a Msg> {
    messages
        .iter()
        .rev()
        .find(|e| e.body.kind == "edit" && e.body.reply_to.as_deref() == Some(id))
}

/// A message's text, with a faint "(edited)" after it once edited.
fn edited_text(text: &str, edited: bool) -> gpui::StyledText {
    if !edited {
        return gpui::StyledText::new(text.to_string());
    }
    let mark = "\u{a0}(edited)";
    let full = format!("{text}{mark}");
    let range = text.len()..full.len();
    gpui::StyledText::new(full).with_highlights([(
        range,
        gpui::HighlightStyle {
            color: Some(theme::text_faint()),
            ..Default::default()
        },
    )])
}

/// How long until a time, in words: "in a moment", "in 4 minutes".
fn until(at: u64) -> String {
    until_from(at, now())
}

fn until_from(at: u64, now: u64) -> String {
    let secs = at.saturating_sub(now);
    match secs {
        0..60 => "in a moment".into(),
        60..120 => "in a minute".into(),
        120..3600 => format!("in {} minutes", secs.div_ceil(60)),
        3600..7200 => "in an hour".into(),
        7200..86_400 => format!("in {} hours", secs / 3600),
        86_400..172_800 => "tomorrow".into(),
        _ => format!("in {} days", secs / 86_400),
    }
}

/// The day a key was made: "today", "yesterday" or "Nov 12".
fn made_on(at: u64) -> String {
    let day = day_label(at);
    match day.as_str() {
        "Today" | "Yesterday" => day.to_lowercase(),
        _ => day,
    }
}

/// The divider over a day's messages: today, yesterday, or the date.
fn day_label(at: u64) -> String {
    let date = |at| crate::platform::local_time(at).map(|t| (t.year, t.month, t.day));
    let (Some(day), Some(today)) = (date(at), date(now())) else {
        return String::new();
    };
    day_label_on(day, today)
}

/// `day_label` for a date seen on the date `today`, both as (year, month
/// from 0, day).
fn day_label_on(day: (i32, i32, i32), today: (i32, i32, i32)) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (year, month, date) = day;
    if day == today {
        "Today".into()
    } else if day == day_before(today) {
        "Yesterday".into()
    } else if year == today.0 {
        format!("{} {date}", MONTHS[month.clamp(0, 11) as usize])
    } else {
        format!("{} {date}, {year}", MONTHS[month.clamp(0, 11) as usize])
    }
}

/// Whether an acceptance has no row of its own: it shows as the "Decided"
/// line under the answer it accepts, or while on its way as a note there.
/// One that names no answer shown here keeps its row, or any member could
/// post a message nobody sees. A refused one keeps it too, so its error and
/// Send again show.
fn accept_shown_under(
    m: &Msg,
    messages: &[Msg],
    decided: &HashMap<String, Vec<super::decisions::Acceptance>>,
) -> bool {
    let Some(of) = super::decisions::accepting(m) else {
        return false;
    };
    match m.delivery {
        Delivery::Sent => decided.contains_key(of),
        Delivery::Sending => messages
            .iter()
            .any(|a| a.id == of && a.delivery == Delivery::Sent && super::bot::meta(a).is_some()),
        Delivery::Failed(_) => false,
    }
}

/// The date before a date, by the calendar. A day is not always 24 hours:
/// the local time 24 hours back is two dates back in the first hour after a
/// 23-hour day.
fn day_before((year, month, date): (i32, i32, i32)) -> (i32, i32, i32) {
    if date > 1 {
        return (year, month, date - 1);
    }
    let (year, month) = if month == 0 {
        (year - 1, 11)
    } else {
        (year, month - 1)
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 if leap => 29,
        1 => 28,
        3 | 5 | 8 | 10 => 30,
        _ => 31,
    };
    (year, month, days)
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
    fn an_acceptance_hides_only_under_an_answer_shown_here() {
        let msg = |id: &str, body: super::super::crypto::Body, delivery| Msg {
            id: id.into(),
            seq: None,
            at: 1,
            github_id: 7,
            login: "dani".into(),
            body,
            delivery,
            edited: None,
        };
        let mut answer_body = super::super::crypto::Body::text("", None);
        answer_body.kind = "bot".into();
        answer_body.bot = Some(serde_json::json!({ "via": 7, "range": ["m1", "m2"], "cites": {} }));
        let answer = msg("b1", answer_body, Delivery::Sent);
        let accept = |of: &str, delivery| {
            let mut body = super::super::bot::accept_body(&answer);
            body.decision = Some(serde_json::json!({ "of": of }));
            msg("a1", body, delivery)
        };
        let messages = vec![answer.clone()];
        let decided = HashMap::from([("b1".to_string(), Vec::new())]);
        assert!(accept_shown_under(
            &accept("b1", Delivery::Sent),
            &messages,
            &decided
        ));
        // It names an answer this device cannot show, or none at all.
        let nothing = HashMap::new();
        assert!(!accept_shown_under(
            &accept("b1", Delivery::Sent),
            &messages,
            &nothing
        ));
        assert!(!accept_shown_under(
            &accept("gone", Delivery::Sending),
            &messages,
            &nothing
        ));
        assert!(accept_shown_under(
            &accept("b1", Delivery::Sending),
            &messages,
            &nothing
        ));
        assert!(!accept_shown_under(
            &accept("b1", Delivery::Failed("refused".into())),
            &messages,
            &decided
        ));
    }

    #[test]
    fn a_remote_environment_is_no_project_for_the_bot() {
        let here = std::path::PathBuf::from("/home/me/code/app");
        assert_eq!(TeamsView::local_project(here.clone()), Some(here));
        let there = crate::model::Remote::project_path("app");
        assert_eq!(TeamsView::local_project(there), None);
    }

    #[test]
    fn until_reads_as_words() {
        assert_eq!(until_from(100, 100), "in a moment");
        assert_eq!(until_from(190, 100), "in a minute");
        assert_eq!(until_from(100 + 250, 100), "in 5 minutes");
        assert_eq!(until_from(100 + 3 * 3600, 100), "in 3 hours");
        assert_eq!(until_from(50, 100), "in a moment");
    }

    #[test]
    fn a_channel_is_viewed_only_while_the_tab_is_on_screen() {
        let channel = Pane::Channel("t1".into(), "c1".into());
        assert_eq!(viewed(&channel, true), Some(hub::channel_key("t1", "c1")));
        // Behind another tab, what arrives stays unread.
        assert_eq!(viewed(&channel, false), None);
        assert_eq!(viewed(&Pane::Team("t1".into()), true), None);
        assert_eq!(viewed(&Pane::None, true), None);
    }

    #[test]
    fn remind_waits_an_hour_after_the_last_reminder() {
        // Never reminded, never refused.
        assert_eq!(remind_wait(None, None, 5000), None);
        // Just reminded: the button waits before the server has to refuse.
        assert_eq!(remind_wait(None, Some(5000), 5000), Some(8600));
        assert_eq!(remind_wait(None, Some(5000), 8599), Some(8600));
        assert_eq!(remind_wait(None, Some(5000), 8600), None);
        // The server's own time wins when it is later, and ends too.
        assert_eq!(remind_wait(Some(9000), Some(5000), 5000), Some(9000));
        assert_eq!(remind_wait(Some(6000), Some(5000), 5000), Some(8600));
        assert_eq!(remind_wait(Some(9000), None, 9000), None);
    }

    #[test]
    fn switching_members_or_signing_out_discards_shared_channel_drafts() {
        let mut drafts = Drafts::default();
        assert!(drafts.switch(Some(7)));
        drafts
            .channels
            .insert("team/channel".into(), "private draft".into());
        assert!(!drafts.switch(Some(7)));
        assert_eq!(drafts.channels["team/channel"], "private draft");
        assert!(drafts.switch(Some(8)));
        assert!(drafts.channels.is_empty());
        drafts
            .channels
            .insert("team/channel".into(), "another draft".into());
        assert!(drafts.switch(None));
        assert!(drafts.switch(Some(7)));
        assert!(drafts.channels.is_empty());
    }

    #[test]
    fn a_draft_is_kept_for_the_channel_the_view_leaves() {
        let mut drafts = Drafts::default();
        let channel = Pane::Channel("team".into(), "channel".into());
        let key = hub::channel_key("team", "channel");
        drafts.keep(&channel, "half a thought");
        assert_eq!(drafts.channels[&key], "half a thought");
        // Nothing typed does not wipe what was kept before.
        drafts.keep(&channel, "  ");
        assert_eq!(drafts.channels[&key], "half a thought");
        // Other panes have no composer.
        drafts.keep(&Pane::Team("team".into()), "not a draft");
        assert_eq!(drafts.channels.len(), 1);
    }

    #[test]
    fn a_created_team_opens_only_from_the_form() {
        assert!(Pane::NewTeam.opens_created());
        // The first team's form shows on the empty pane.
        assert!(Pane::None.opens_created());
        for pane in [
            Pane::Channel("a".into(), "general".into()),
            Pane::Team("a".into()),
            Pane::Invite("a".into()),
            Pane::Pending("a".into()),
            Pane::Invites,
            Pane::Asked("a".into()),
            Pane::Devices,
        ] {
            assert!(!pane.opens_created(), "{pane:?}");
        }
    }

    #[test]
    fn start_a_team_is_offered_over_an_invite_or_a_wait() {
        // An invite or a wait shows on the empty pane in place of the form.
        assert!(Pane::None.offers_start(true, true));
        assert!(Pane::Invites.offers_start(true, false));
        assert!(Pane::Asked("a".into()).offers_start(true, false));
        // The empty pane with nothing waiting is the form itself.
        assert!(!Pane::None.offers_start(true, false));
        assert!(!Pane::NewTeam.offers_start(true, true));
        assert!(!Pane::Devices.offers_start(true, true));
        // With a team, the team menu has it.
        assert!(!Pane::None.offers_start(false, true));
        assert!(!Pane::Invites.offers_start(false, true));
    }

    #[test]
    fn a_new_channel_goes_only_to_the_team_its_field_is_for() {
        assert_eq!(new_channel_team(Some("a"), Some("a")), Some("a".into()));
        // Typed for team a, then the sheet moved to team b.
        assert_eq!(new_channel_team(Some("a"), Some("b")), None);
        assert_eq!(new_channel_team(None, Some("b")), None);
        assert_eq!(new_channel_team(Some("a"), None), None);
    }

    #[test]
    fn device_notices_go_by_when_the_device_was_added() {
        let notice = |team: &str, id: &str| Notice {
            team_id: team.into(),
            login: "rik".into(),
            device: id.into(),
            device_id: id.into(),
        };
        // The roster's order, not the order the devices were added in.
        let notices = [
            notice("t", "late"),
            notice("t", "gone"),
            notice("other", "elsewhere"),
            notice("t", "early"),
        ];
        let added: HashMap<String, u64> =
            [("late".to_string(), 200), ("early".to_string(), 100)].into();
        let order: Vec<(String, u64)> = notices_in_order(&notices, "t", &added)
            .into_iter()
            .map(|(n, at)| (n.device_id, at))
            .collect();
        assert_eq!(
            order,
            [
                ("early".to_string(), 100),
                ("late".to_string(), 200),
                ("gone".to_string(), u64::MAX),
            ]
        );
    }

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
    fn yesterday_is_the_calendar_day_before() {
        assert_eq!(day_before((2026, 2, 30)), (2026, 2, 29));
        assert_eq!(day_before((2026, 2, 1)), (2026, 1, 28));
        assert_eq!(day_before((2024, 2, 1)), (2024, 1, 29));
        assert_eq!(day_before((2026, 4, 1)), (2026, 3, 30));
        assert_eq!(day_before((2026, 0, 1)), (2025, 11, 31));
        // The day the clocks go forward has 23 hours. Whatever the hour on
        // the day after, the short day is yesterday, not the one before it.
        let today = (2026, 2, 30);
        assert_eq!(day_label_on(today, today), "Today");
        assert_eq!(day_label_on((2026, 2, 29), today), "Yesterday");
        assert_eq!(day_label_on((2026, 2, 28), today), "Mar 28");
        assert_eq!(day_label_on((2025, 11, 31), (2026, 0, 1)), "Yesterday");
        assert_eq!(day_label_on((2025, 11, 30), (2026, 0, 1)), "Dec 30, 2025");
    }

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(plural(1, "device", "devices"), "1 device");
        assert_eq!(plural(3, "person", "people"), "3 people");
    }

    #[test]
    fn the_newest_edit_is_found() {
        let msg = |id: &str, kind: &str, reply_to: Option<&str>, delivery| {
            let mut body = crate::arakun_teams::crypto::Body::text("x", reply_to.map(Into::into));
            body.kind = kind.into();
            Msg {
                id: id.into(),
                seq: None,
                at: 0,
                github_id: 1,
                login: "rik".into(),
                body,
                delivery,
                edited: None,
            }
        };
        let ms = [
            msg("a", "text", None, Delivery::Sent),
            msg("e1", "edit", Some("a"), Delivery::Sent),
            msg("b", "text", Some("a"), Delivery::Sent),
            msg("e2", "edit", Some("a"), Delivery::Failed("offline".into())),
        ];
        let e = last_edit(&ms, "a").unwrap();
        assert_eq!(e.id, "e2");
        assert!(matches!(e.delivery, Delivery::Failed(_)));
        assert!(last_edit(&ms, "b").is_none());
        // A refused reversal keeps its row; an edit never has one.
        // Acceptances are left to `accept_shown_under`.
        let failed = || Delivery::Failed("refused".into());
        for (kind, delivery, hidden) in [
            ("text", Delivery::Sent, false),
            ("edit", Delivery::Sent, true),
            ("edit", failed(), true),
            ("decision_accept", Delivery::Sending, false),
            ("decision_reverse", Delivery::Sent, true),
            ("decision_reverse", Delivery::Sending, true),
            ("decision_reverse", failed(), false),
        ] {
            assert_eq!(
                hidden_row(&msg("x", kind, None, delivery)),
                hidden,
                "{kind}"
            );
        }
    }
}
