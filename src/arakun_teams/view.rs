//! The Teams tab: the user's teams with their channels on the left, and on
//! the right a channel's chat, a team's members, invites and approvals, or a
//! form. What it shows comes from the [`Hub`]; this view only draws it and
//! keeps what is typed.

use std::collections::HashMap;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window, div, px,
};

use super::Teams;
use super::api::{InviteRow, Role};
use super::hub::{self, ChannelState, Delivery, DeviceState, Hub, Load, Msg, TeamState};
use super::socket;
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

pub enum TeamsViewEvent {
    /// Settings → Account, to sign in again.
    OpenSettings,
}

/// What the side beside the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pane {
    /// Nothing picked yet: the first channel once there is one.
    None,
    Channel(String, String),
    Team(String),
    NewTeam,
    Devices,
}

/// A team as the list shows it.
struct ListTeam {
    id: String,
    name: String,
    role: Role,
    /// Each channel's ID and name, `None` while locked.
    channels: Vec<(String, Option<String>)>,
    /// People waiting for approval.
    pending: usize,
    keyless: bool,
}

/// The list of teams, as wide as a sidebar.
const LIST_W: f32 = 248.;
/// Messages from one sender this close together share a header.
const GROUP_SECS: u64 = 5 * 60;
const AVATAR: f32 = 28.;
/// How much of a message a reply quotes.
const SNIPPET: usize = 90;

pub struct TeamsView {
    teams: Entity<Teams>,
    hub: Entity<Hub>,
    pane: Pane,
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
    /// Messages in the open channel at the last render, and whether the
    /// list sat at its bottom: new ones then keep it there.
    seen_count: usize,
    /// A message just jumped to, lit for a moment.
    flash: Option<String>,
    _flash: Option<Task<()>>,
    /// "Remove" or "Leave" was pressed once on this member.
    confirm_remove: Option<(String, u64)>,
    copied: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TeamsViewEvent> for TeamsView {}

impl TeamsView {
    pub fn new(teams: Entity<Teams>, cx: &mut Context<Self>) -> Self {
        let hub = cx.new(|cx| Hub::new(teams.clone(), cx));
        let team_name = cx.new(|cx| TextInput::new("Team name", cx));
        let invite = cx.new(|cx| TextInput::new("GitHub user name", cx));
        let channel_name = cx.new(|cx| TextInput::new("New channel", cx));
        let composer = cx.new(|cx| TextInput::new("Message", cx));
        let device_code = cx.new(|cx| TextInput::new("arakun-device.…", cx).mono());
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
            on(cx, &invite, |this, ev, cx| {
                if ev == InputEvent::Submit {
                    this.send_invite(cx)
                }
            }),
            on(cx, &channel_name, |this, ev, cx| {
                if ev == InputEvent::Submit {
                    this.create_channel(cx)
                }
            }),
            on(cx, &composer, |this, ev, cx| match ev {
                InputEvent::Submit => this.send(cx),
                InputEvent::Cancel => {
                    this.reply_to = None;
                    cx.notify();
                }
                _ => {}
            }),
            on(cx, &device_code, |_, _, cx| cx.notify()),
        ];
        Self {
            teams,
            hub,
            pane: Pane::None,
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
            copied: false,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    /// Opens the first channel once there is one and nothing else was picked,
    /// and keeps the pane on things that still exist.
    fn hub_changed(&mut self, cx: &mut Context<Self>) {
        let hub = self.hub.read(cx);
        let gone = match &self.pane {
            Pane::Channel(t, c) => !hub
                .team
                .get(t)
                .is_some_and(|s| s.channels.iter().any(|ch| ch.id == *c)),
            Pane::Team(t) => !hub.teams.iter().any(|r| r.team_id == *t),
            _ => false,
        };
        // A team still loading is not gone yet.
        let loading = match &self.pane {
            Pane::Channel(t, _) | Pane::Team(t) => {
                hub.teams.iter().any(|r| r.team_id == *t) && !hub.team.contains_key(t)
            }
            _ => false,
        };
        if gone && !loading {
            self.pane = Pane::None;
        }
        if self.pane == Pane::None {
            let first = hub.teams.iter().find_map(|r| {
                let t = hub.team.get(&r.team_id)?;
                let c = t.channels.first()?;
                Some((r.team_id.clone(), c.id.clone()))
            });
            if let Some((t, c)) = first {
                self.open_channel(t, c, cx);
            }
        }
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
        let draft = self.drafts.remove(&key).unwrap_or_default();
        self.composer.update(cx, |i, cx| i.set_text(draft, cx));
        self.reply_to = None;
        self.seen_count = 0;
        self.flash = None;
        self.hub
            .update(cx, |h, cx| h.open_channel(&team, &channel, cx));
        self.pane = Pane::Channel(team, channel);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Shows a pane, with the cursor in its field when it has one.
    fn show(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let field = match &pane {
            Pane::NewTeam => Some(self.team_name.clone()),
            Pane::Devices => Some(self.device_code.clone()),
            Pane::Channel(..) => Some(self.composer.clone()),
            _ => None,
        };
        if let Pane::Channel(t, c) = pane {
            self.open_channel(t, c, cx);
        } else {
            self.pane = pane;
        }
        if let Some(field) = field {
            let handle = field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn selected_team(&self) -> Option<&str> {
        match &self.pane {
            Pane::Channel(t, _) | Pane::Team(t) => Some(t),
            _ => None,
        }
    }

    fn create_team(&mut self, cx: &mut Context<Self>) {
        let name = self.team_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.team_name.update(cx, |i, cx| i.set_text("", cx));
        self.hub.update(cx, |h, cx| h.create_team(name, cx));
        self.pane = Pane::None;
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
        self.hub
            .update(cx, |h, cx| h.create_channel(&team, name, cx));
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

    // ---- pieces ------------------------------------------------------------------

    fn banner(icon_name: &str, tone: Tone, text: impl Into<SharedString>) -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(8.))
            .px(ui::sp(12.))
            .py(ui::sp(6.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(icon(icon_name, 14.).text_color(tone.color()))
            .child(ui::text(Type::Label, tone.color(), text).flex_1().min_w_0())
    }

    fn section(title: &str) -> gpui::Div {
        ui::overline(title).pt(ui::sp(4.))
    }

    fn list_row(id: impl Into<gpui::ElementId>, selected: bool) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .min_h(px(ui::MENU_ROW_H))
            .px(ui::sp(8.))
            .rounded(ui::rad(ui::RADIUS))
            .cursor_pointer()
            .hover(|d| d.bg(theme::fill_hover()))
            .when(selected, |d| d.bg(theme::fill_selected()))
    }

    // ---- the list ------------------------------------------------------------------

    fn render_invite(
        &self,
        ix: usize,
        inv: &InviteRow,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let inv2 = inv.clone();
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(6.))
            .p(ui::sp(8.))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(
                div()
                    .truncate()
                    .ty(Type::Label)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::content())
                    .child(inv.team_name.clone()),
            )
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                match &inv.invited_by_login {
                    Some(by) => format!("@{by} invited you"),
                    None => "You're invited".into(),
                },
            ))
            .child(
                div().flex().child(
                    ui::button(
                        ("teams-accept", ix),
                        if busy { "Joining…" } else { "Accept" },
                        ButtonKind::Primary,
                        !busy,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.hub.update(cx, |h, cx| h.accept(&inv2, cx));
                    })),
                ),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let invites: Vec<InviteRow> = hub.invites.clone();
        let invite_busy: Vec<bool> = invites
            .iter()
            .map(|i| hub.busy.contains(&format!("accept:{}", i.invite_id)))
            .collect();
        let asked = hub.asked();
        let creating = hub.busy.contains("create-team");
        let teams: Vec<ListTeam> = hub
            .teams
            .iter()
            .map(|r| {
                let t = hub.team.get(&r.team_id);
                ListTeam {
                    id: r.team_id.clone(),
                    name: r.name.clone(),
                    role: r.role,
                    channels: t
                        .map(|t| {
                            t.channels
                                .iter()
                                .map(|c| (c.id.clone(), c.name.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    pending: t.map_or(0, |t| t.pending.len()),
                    keyless: t.is_some_and(|t| t.keyless),
                }
            })
            .collect();
        let open = match &self.pane {
            Pane::Channel(t, c) => Some((t.clone(), c.clone())),
            _ => None,
        };
        let selected_team = self.selected_team().map(str::to_string);

        let mut list = div()
            .id("teams-list")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_W))
            .gap(ui::sp(4.))
            .p(ui::sp(8.))
            .border_r_1()
            .border_color(theme::border_subtle())
            .overflow_y_scroll();

        if !invites.is_empty() {
            list = list
                .child(Self::section("Invites").px(ui::sp(8.)))
                .children(
                    invites
                        .iter()
                        .enumerate()
                        .map(|(ix, inv)| self.render_invite(ix, inv, invite_busy[ix], cx)),
                );
        }

        list = list.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(ui::sp(8.))
                .pt(ui::sp(6.))
                .child(Self::section("Teams"))
                .child(
                    ui::icon_button("teams-new", "add01", 14.)
                        .tooltip(ui::tip("New team"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.show(Pane::NewTeam, window, cx)),
                        ),
                ),
        );
        if creating {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .px(ui::sp(8.))
                    .h(px(ui::MENU_ROW_H))
                    .child(ui::spinner("teams-creating", 12., theme::text_faint()))
                    .child(ui::text(
                        Type::Body,
                        theme::text_subtle(),
                        "Creating the team…",
                    )),
            );
        }
        for (ix, team) in teams.into_iter().enumerate() {
            let ListTeam {
                id: team_id,
                name,
                role,
                channels,
                pending,
                keyless,
            } = team;
            let selected = selected_team.as_deref() == Some(team_id.as_str());
            let tid = team_id.clone();
            list = list.child(
                Self::list_row(
                    ("teams-team", ix),
                    matches!(&self.pane, Pane::Team(t) if *t == team_id),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pane = Pane::Team(tid.clone());
                    let tid = tid.clone();
                    this.hub.update(cx, |h, cx| h.load_team(&tid, cx));
                    cx.notify();
                }))
                .child(ui::avatar(&name, 20.).rounded(ui::rad(ui::RADIUS_SM)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .ty(Type::Label)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::content())
                        .child(name),
                )
                .when(pending > 0, |d| {
                    d.child(ui::badge(format!("{pending} waiting"), Tone::Warning))
                })
                .when(role != Role::Member && pending == 0, |d| {
                    d.child(ui::text(Type::Caption, theme::text_faint(), role.word()))
                }),
            );
            if keyless {
                list = list.child(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        "Waiting for an admin to approve this device",
                    )
                    .pl(ui::sp(36.))
                    .pr(ui::sp(8.))
                    .pb(ui::sp(4.)),
                );
            }
            if selected || open.is_none() {
                for (cix, (cid, cname)) in channels.into_iter().enumerate() {
                    let on = open
                        .as_ref()
                        .is_some_and(|(t, c)| *t == team_id && *c == cid);
                    let (t2, c2) = (team_id.clone(), cid.clone());
                    list = list.child(
                        Self::list_row(("teams-channel", ix * 1000 + cix), on)
                            .pl(ui::sp(36.))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show(Pane::Channel(t2.clone(), c2.clone()), window, cx)
                            }))
                            .child(ui::text(Type::Body, theme::text_faint(), "#"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Body)
                                    .text_color(if on {
                                        theme::content()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(cname.unwrap_or_else(|| "locked channel".into())),
                            ),
                    );
                }
            }
        }
        for (team_id, name) in asked {
            list = list.child(
                div()
                    .id(SharedString::from(format!("teams-asked-{team_id}")))
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .min_h(px(ui::MENU_ROW_H))
                    .px(ui::sp(8.))
                    .child(
                        ui::avatar(&name, 20.)
                            .rounded(ui::rad(ui::RADIUS_SM))
                            .opacity(0.5),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Label)
                                    .text_color(theme::text_subtle())
                                    .child(name),
                            )
                            .child(ui::text(
                                Type::Caption,
                                theme::text_faint(),
                                "Waiting for an admin to approve you",
                            )),
                    ),
            );
        }
        list.into_any_element()
    }

    // ---- panes -----------------------------------------------------------------------

    fn page() -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(ui::PAGE_PAD))
            .max_w(px(640.))
    }

    fn render_new_team(&self, cx: &mut Context<Self>) -> AnyElement {
        let creating = self.hub.read(cx).busy.contains("create-team");
        Self::page()
            .child(ui::page_title(
                "New team",
                Some("A place to talk with the people you build with.".into_any_element()),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(ui::field().flex_1().child(self.team_name.clone()))
                    .child(
                        ui::button("teams-create", if creating { "Creating…" } else { "Create" }, ButtonKind::Primary, !creating)
                            .on_click(cx.listener(|this, _, _, cx| this.create_team(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(ui::sp(8.))
                    .child(icon("square-lock02", 14.).text_color(theme::text_faint()).mt(px(1.)))
                    .child(
                        ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            "Channel names and messages are encrypted on this device. The server keeps the team's name, who is in it and when messages were sent, never what they say. You can invite people by their GitHub user name once the team exists.",
                        )
                        .flex_1()
                        .min_w_0(),
                    ),
            )
            .into_any_element()
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
        let rows = devices.iter().map(|d| {
            let this_one = d.device_id == mine;
            div()
                .flex()
                .items_center()
                .gap(ui::sp(10.))
                .py(ui::sp(6.))
                .child(icon("computer", 16.).text_color(theme::text_subtle()))
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
                                .text_color(theme::content())
                                .child(crate::redact::text(&d.name).into_owned()),
                        )
                        .child(ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            format!("{} · added {}", short_id(&d.device_id), when(d.created_at)),
                        )),
                )
                .when(this_one, |r| {
                    r.child(ui::badge("this device", Tone::Neutral))
                })
        });
        Self::page()
            .child(ui::page_title(
                "Your devices",
                Some("Every device signs what it sends, and holds its own copy of each team key.".into_any_element()),
            ))
            .child(div().flex().flex_col().children(rows))
            .child(Self::section("Add a device"))
            .child(ui::text(
                Type::Body,
                theme::text_subtle(),
                "On the new device, open Teams: it shows a code. Paste it here to vouch for that device. Your teams' admins then give it the keys.",
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
                    if known { theme::text_faint() } else { theme::text_subtle() },
                    if known {
                        format!("{} is one of your devices already.", crate::redact::text(&c.name))
                    } else {
                        format!(
                            "\"{}\", device {}. Check the new device shows the same ID.",
                            crate::redact::text(&c.name),
                            short_id(&c.id)
                        )
                    },
                )
            }))
            .when(!code_text.trim().is_empty() && hub::read_device_code(&code_text).is_none(), |d| {
                d.child(ui::text(
                    Type::Caption,
                    theme::attention().into(),
                    "That is not a device code. Copy the whole line the new device shows.",
                ))
            })
            .into_any_element()
    }

    fn render_team(&self, team_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(t) = hub.team.get(team_id) else {
            return ui::loading("teams-team-loading", "Reading the team…").into_any_element();
        };
        let me = hub.me().map_or(0, |m| m.github_id);
        let admin = t.role.is_admin();
        let inviting = hub.busy.contains(&format!("invite:{team_id}"));
        let making_channel = hub.busy.contains(&format!("channel:{team_id}"));
        let notices: Vec<_> = hub
            .notices
            .iter()
            .filter(|n| n.team_id == team_id)
            .cloned()
            .collect();
        let members = t.detail.members.clone();
        let pending = t.pending.clone();
        let channels = t.channels.clone();
        let devices: HashMap<u64, usize> = t.devices.iter().map(|(k, v)| (*k, v.len())).collect();
        let approving: Vec<bool> = pending
            .iter()
            .map(|p| {
                hub.busy
                    .contains(&format!("approve:{team_id}:{}", p.github_id))
            })
            .collect();
        let keyless = t.keyless;
        let problem = t.problem.clone();
        let name = t.detail.name.clone();
        let epoch = t.detail.epoch;
        let role = t.role;

        let mut page = Self::page().child(ui::page_title(
            name,
            Some(
                format!(
                    "{} member{} · you are {} · key {epoch}",
                    members.len(),
                    if members.len() == 1 { "" } else { "s" },
                    if role == Role::Admin {
                        "an admin"
                    } else if role == Role::Owner {
                        "the owner"
                    } else {
                        "a member"
                    }
                )
                .into_any_element(),
            ),
        ));
        if keyless {
            page = page.child(
                ui::card()
                    .flex()
                    .items_center()
                    .gap(ui::sp(10.))
                    .p(ui::sp(12.))
                    .child(ui::spinner("teams-keyless", 14., theme::text_faint()))
                    .child(ui::text(
                        Type::Body,
                        theme::text(),
                        "Waiting for an admin to approve this device. Messages show here once one of their devices hands it the team key.",
                    )),
            );
        }
        if let Some(p) = problem {
            page = page.child(ui::text(Type::Label, theme::attention().into(), p));
        }
        for (i, n) in notices.into_iter().enumerate() {
            let id = n.device_id.clone();
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(icon("computer", 14.).text_color(theme::text_subtle()))
                    .child(
                        ui::text(
                            Type::Label,
                            theme::text(),
                            format!(
                                "@{} added a new device: {}",
                                n.login,
                                crate::redact::text(&n.device)
                            ),
                        )
                        .flex_1(),
                    )
                    .child(
                        ui::icon_button(("teams-notice-team", i), "cancel01", 12.)
                            .tooltip(ui::tip("Dismiss"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.hub.update(cx, |h, cx| h.dismiss_notice(&id, cx))
                            })),
                    ),
            );
        }

        // Channels.
        let tid = team_id.to_string();
        page =
            page.child(Self::section("Channels")).child(
                div().flex().flex_col().gap(ui::sp(2.)).children(
                    channels.into_iter().enumerate().map(|(i, c)| {
                        let (t2, c2) = (tid.clone(), c.id.clone());
                        Self::list_row(("teams-team-channel", i), false)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show(Pane::Channel(t2.clone(), c2.clone()), window, cx)
                            }))
                            .child(ui::text(Type::Body, theme::text_faint(), "#"))
                            .child(ui::text(
                                Type::Body,
                                theme::text(),
                                c.name.unwrap_or_else(|| {
                                    "locked: sealed with a key this device doesn't hold".into()
                                }),
                            ))
                    }),
                ),
            );
        if admin && !keyless {
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(
                        ui::field()
                            .flex_1()
                            .child(ui::text(Type::Body, theme::text_faint(), "#"))
                            .child(self.channel_name.clone()),
                    )
                    .child(
                        ui::button(
                            "teams-add-channel",
                            if making_channel {
                                "Adding…"
                            } else {
                                "Add channel"
                            },
                            ButtonKind::Outline,
                            !making_channel,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.create_channel(cx))),
                    ),
            );
        }

        // Waiting for approval.
        if admin && !pending.is_empty() {
            page = page.child(Self::section("Waiting for approval"));
            for (i, p) in pending.iter().enumerate() {
                let (t1, t2, gid) = (tid.clone(), tid.clone(), p.github_id);
                let busy = approving[i];
                page = page.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(10.))
                        .p(ui::sp(10.))
                        .rounded(ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::fill_raised())
                        .child(ui::avatar(&p.login, AVATAR))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    ui::text(
                                        Type::Label,
                                        theme::content(),
                                        format!("@{}", p.login),
                                    )
                                    .font_weight(FontWeight::SEMIBOLD),
                                )
                                .child(ui::text(
                                    Type::Caption,
                                    theme::text_faint(),
                                    format!(
                                        "accepted the invite {} · {} device{}",
                                        when(p.requested_at),
                                        p.devices.len(),
                                        if p.devices.len() == 1 { "" } else { "s" }
                                    ),
                                )),
                        )
                        .child(
                            ui::button(("teams-reject", i), "Turn down", ButtonKind::Ghost, !busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.hub.update(cx, |h, cx| h.reject(&t1, gid, cx))
                                })),
                        )
                        .child(
                            ui::button_with(
                                ("teams-approve", i),
                                Some("tick02"),
                                if busy { "Approving…" } else { "Approve" },
                                ButtonKind::Primary,
                                !busy && !keyless,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.hub.update(cx, |h, cx| h.approve(&t2, gid, cx))
                                },
                            )),
                        ),
                );
            }
            page = page.child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "Approving seals the team's keys to each of their devices on this one. The server can't let anyone in by itself.",
            ));
        }

        // Members.
        let owners = members.iter().filter(|m| m.role == Role::Owner).count();
        page = page.child(Self::section("Members"));
        for (i, m) in members.iter().enumerate() {
            let confirming = self.confirm_remove.as_ref() == Some(&(tid.clone(), m.github_id));
            let last_owner = m.role == Role::Owner && owners < 2;
            let can_remove = !last_owner
                && (m.github_id == me || (admin && m.role != Role::Owner) || role == Role::Owner);
            let gid = m.github_id;
            let (t1, t2) = (tid.clone(), tid.clone());
            let self_row = m.github_id == me;
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(10.))
                    .py(ui::sp(4.))
                    .child(ui::avatar(&m.login, AVATAR))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(ui::text(
                                Type::Label,
                                theme::content(),
                                format!("@{}", m.login),
                            ))
                            .child(ui::text(
                                Type::Caption,
                                theme::text_faint(),
                                format!(
                                    "joined {} · {} device{}",
                                    when(m.joined_at),
                                    devices.get(&m.github_id).copied().unwrap_or(0),
                                    if devices.get(&m.github_id) == Some(&1) {
                                        ""
                                    } else {
                                        "s"
                                    }
                                ),
                            )),
                    )
                    .when(m.role != Role::Member, |d| {
                        d.child(ui::badge(m.role.word(), Tone::Neutral))
                    })
                    .when(can_remove && !confirming, |d| {
                        d.child(
                            ui::button(
                                ("teams-remove", i),
                                if self_row { "Leave" } else { "Remove" },
                                ButtonKind::Ghost,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.confirm_remove = Some((t1.clone(), gid));
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(confirming, |d| {
                        d.child(
                            ui::button(
                                ("teams-remove-yes", i),
                                if self_row { "Leave the team" } else { "Remove" },
                                ButtonKind::Danger,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.confirm_remove = None;
                                    this.hub.update(cx, |h, cx| h.remove(&t2, gid, cx));
                                },
                            )),
                        )
                        .child(
                            ui::button(("teams-remove-no", i), "Keep", ButtonKind::Ghost, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_remove = None;
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }

        if admin {
            page = page
                .child(Self::section("Invite"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(ui::field().flex_1().child(ui::text(Type::Body, theme::text_faint(), "@")).child(self.invite.clone()))
                        .child(
                            ui::button_with("teams-invite", Some("user"), if inviting { "Inviting…" } else { "Invite" }, ButtonKind::Primary, !inviting)
                                .on_click(cx.listener(|this, _, _, cx| this.send_invite(cx))),
                        ),
                )
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    "They accept it in their Teams tab, then you approve them here. An invite lasts 7 days.",
                ));
        }
        div()
            .id("teams-team-page")
            .size_full()
            .overflow_y_scroll()
            .child(page)
            .into_any_element()
    }

    // ---- the chat -----------------------------------------------------------------------

    fn render_message(
        &self,
        index: usize,
        m: &Msg,
        compact: bool,
        quoted: Option<(usize, &Msg)>,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group: SharedString = format!("teams-msg-{index}").into();
        let id = m.id.clone();
        let lit = self.flash.as_deref() == Some(m.id.as_str())
            || self.reply_to.as_deref() == Some(m.id.as_str());
        let quote = m.body.reply_to.as_ref().map(|target| match quoted {
            Some((qi, q)) => {
                let qid = q.id.clone();
                div()
                    .id(("teams-quote", index))
                    .flex()
                    .items_center()
                    .gap(ui::sp(6.))
                    .pl(ui::sp(AVATAR / 2.))
                    .min_w_0()
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(AVATAR / 2. + 4.))
                            .h(px(8.))
                            .mt(px(6.))
                            .border_l_2()
                            .border_t_2()
                            .border_color(theme::border_strong())
                            .rounded_tl(ui::rad(ui::RADIUS_SM)),
                    )
                    .child(ui::avatar(&q.login, 14.))
                    .child(
                        ui::text(Type::Caption, theme::text_subtle(), format!("@{}", q.login))
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD),
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
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(qi, qid.clone(), cx)))
                    .into_any_element()
            }
            None => div()
                .flex()
                .items_center()
                .gap(ui::sp(6.))
                .pl(ui::sp(AVATAR + 12.))
                .child(icon("arrow-turn-backward", 11.).text_color(theme::text_faint()))
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
        let compact = compact && quote.is_none();
        let kind = match m.body.kind.as_str() {
            "text" => None,
            "bot" => Some("bot"),
            "decision" => Some("decision"),
            "decision_accept" => Some("accepted"),
            other => Some(other),
        }
        .map(|k| ui::badge(k.to_string(), Tone::Info));
        let delivery = match &m.delivery {
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
        };
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
            .when(!compact, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(
                            ui::text(Type::Label, theme::content(), m.login.clone())
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(ui::text(Type::Caption, theme::text_faint(), when(m.at)))
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
            .flex_col()
            .gap(ui::sp(2.))
            .px(ui::sp(16.))
            .pt(ui::sp(if compact { 1. } else { 10. }))
            .pb(ui::sp(1.))
            .hover(|d| d.bg(theme::fill_hover()))
            .when(lit, |d| d.bg(theme::fill_selected()))
            .children(quote)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(ui::sp(12.))
                    .child(if compact {
                        div()
                            .w(px(AVATAR))
                            .flex_none()
                            .invisible()
                            .group_hover(group.clone(), |s| s.visible())
                            .child(
                                ui::text(Type::Caption, theme::text_faint(), clock(m.at))
                                    .pt(ui::sp(2.))
                                    .whitespace_nowrap(),
                            )
                    } else {
                        ui::avatar(&m.login, AVATAR).mt(px(2.))
                    })
                    .child(body),
            )
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

    fn render_channel(
        &mut self,
        team_id: &str,
        channel_id: &str,
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
        let team_name = t.detail.name.clone();
        let notices: Vec<_> = hub
            .notices
            .iter()
            .filter(|n| n.team_id == team_id)
            .cloned()
            .collect();
        let members = t.detail.members.len();
        let reply = self
            .reply_to
            .as_ref()
            .and_then(|id| messages.iter().find(|m| m.id == *id))
            .cloned();

        // Keep the newest in view when it arrives and the list was at its bottom.
        if messages.len() != self.seen_count {
            let max = self.scroll.max_offset().y;
            let at_bottom = -self.scroll.offset().y >= max - px(48.);
            if self.seen_count == 0 || at_bottom {
                self.scroll.scroll_to_bottom();
            }
            self.seen_count = messages.len();
        }

        let (dot, word) = match &status {
            socket::Status::Connected => (Tone::Success, "live".to_string()),
            socket::Status::Connecting => (Tone::Neutral, "connecting…".to_string()),
            socket::Status::Waiting(_) => (Tone::Warning, "reconnecting…".to_string()),
            socket::Status::Refused(why) => (Tone::Danger, why.clone()),
        };
        let tid = team_id.to_string();
        let head = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(10.))
            .h(px(ui::BAR_H))
            .px(ui::sp(16.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(ui::text(Type::Heading, theme::text_faint(), "#"))
            .child(
                div()
                    .truncate()
                    .ty(Type::Label)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::content())
                    .child(name.clone()),
            )
            .child(ui::text(Type::Caption, theme::text_faint(), team_name).truncate())
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(4.))
                    .child(icon("square-lock02", 12.).text_color(theme::text_faint()))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        "end-to-end encrypted",
                    )),
            )
            .child(ui::status_dot(word, dot))
            .child(
                ui::button_with(
                    "teams-members",
                    Some("user"),
                    format!("{members}"),
                    ButtonKind::Ghost,
                    true,
                )
                .tooltip(ui::tip("Members"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pane = Pane::Team(tid.clone());
                    cx.notify();
                })),
            );

        let mut banners: Vec<AnyElement> = Vec::new();
        if keyless {
            banners.push(
                Self::banner(
                    "clock01",
                    Tone::Warning,
                    "Waiting for an admin to approve this device",
                )
                .into_any_element(),
            );
        }
        for (i, n) in notices.into_iter().enumerate() {
            let id = n.device_id.clone();
            banners.push(
                Self::banner(
                    "computer",
                    Tone::Info,
                    format!(
                        "@{} added a new device: {}",
                        n.login,
                        crate::redact::text(&n.device)
                    ),
                )
                .child(
                    ui::icon_button(("teams-notice", i), "cancel01", 12.)
                        .tooltip(ui::tip("Dismiss"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hub.update(cx, |h, cx| h.dismiss_notice(&id, cx))
                        })),
                )
                .into_any_element(),
            );
        }

        // The list: one note at the top, then one child per message, so a
        // message's index is its child's.
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
        let first = if keyless && messages.is_empty() {
            ui::empty_state(
                "square-lock02",
                "Waiting for an admin to approve this device",
                "This device holds no key of the team yet. Once an admin's device hands it one, the messages open here.",
                None,
            )
            .into_any_element()
        } else if loading && messages.is_empty() {
            ui::loading("teams-history", "Reading the channel…").into_any_element()
        } else if messages.is_empty() && hidden + locked == 0 {
            ui::empty_state(
                "message01",
                &format!("This is the start of #{name}"),
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
                .pt(ui::sp(16.))
                .pb(ui::sp(4.))
                .child(ui::text(
                    Type::Heading,
                    theme::content(),
                    format!("#{name}"),
                ))
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    "Messages here are end-to-end encrypted.",
                ))
                .children(
                    note.into_iter()
                        .map(|n| ui::text(Type::Caption, theme::attention().into(), n)),
                )
                .into_any_element()
        };
        let index_of: HashMap<&str, usize> = messages
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.as_str(), i + 1))
            .collect();
        let mut rows: Vec<AnyElement> = vec![first];
        for (i, m) in messages.iter().enumerate() {
            let compact = i > 0 && {
                let p = &messages[i - 1];
                p.github_id == m.github_id
                    && m.at.saturating_sub(p.at) < GROUP_SECS
                    && p.body.kind == m.body.kind
            };
            let quoted = m
                .body
                .reply_to
                .as_deref()
                .and_then(|r| Some((*index_of.get(r)?, messages.iter().find(|q| q.id == r)?)));
            rows.push(self.render_message(i + 1, m, compact, quoted, (team_id, channel_id), cx));
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
        let reply_bar = reply.map(|r| {
            div()
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .px(ui::sp(12.))
                .py(ui::sp(6.))
                .rounded_t(ui::rad(ui::RADIUS))
                .bg(theme::fill_raised())
                .border_1()
                .border_b_0()
                .border_color(theme::border())
                .child(icon("arrow-turn-backward", 12.).text_color(theme::text_subtle()))
                .child(ui::text(Type::Caption, theme::text_subtle(), "Replying to").flex_none())
                .child(
                    ui::text(Type::Caption, theme::content(), format!("@{}", r.login))
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .ty(Type::Caption)
                        .text_color(theme::text_faint())
                        .child(snippet(&r.body.text)),
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
        let composer = div()
            .flex()
            .flex_col()
            .flex_none()
            .px(ui::sp(16.))
            .pb(ui::sp(14.))
            .children(reply_bar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(
                        ui::field()
                            .flex_1()
                            .when(self.reply_to.is_some(), |d| d.rounded_t(ui::rad(0.)))
                            .child(self.composer.clone()),
                    )
                    .child(
                        ui::button_with(
                            "teams-send",
                            Some("arrow-up01"),
                            "Send",
                            ButtonKind::Primary,
                            can_send,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                    ),
            )
            .when(!can_send || !connected, |d| {
                d.child(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        if keyless {
                            "You can write here once this device holds the team key."
                        } else {
                            "Not connected right now. What you send goes out once it is back."
                        },
                    )
                    .pt(ui::sp(6.)),
                )
            });

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(head)
            .children(banners)
            .child(list)
            .child(composer)
            .into_any_element()
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
        Self::centered(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(ui::sp(12.))
                .child(
                    div()
                        .size(px(40.))
                        .rounded(ui::rad(ui::RADIUS))
                        .bg(theme::fill_raised())
                        .border_1()
                        .border_color(theme::border())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("computer", 18.).text_color(theme::text_subtle())),
                )
                .child(ui::heading("Approve this device from your other device"))
                .child(
                    ui::text(
                        Type::Body,
                        theme::text_subtle(),
                        "You use Teams on another device already. There, open Teams › Your devices and paste this code. Only a device of yours can let this one in.",
                    )
                    .text_center(),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .w_full()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .px(ui::sp(10.))
                                .py(ui::sp(6.))
                                .rounded(ui::rad(ui::RADIUS))
                                .bg(theme::fill_raised())
                                .border_1()
                                .border_color(theme::border())
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::text_muted())
                                .child(code.to_string()),
                        )
                        .child(
                            ui::button_with("teams-copy-code", Some(if copied { "tick02" } else { "copy01" }), if copied { "Copied" } else { "Copy" }, ButtonKind::Outline, true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code_owned.clone()));
                                    this.copied = true;
                                    cx.notify();
                                })),
                        ),
                )
                .child(ui::text(Type::Caption, theme::text_faint(), format!("Device ID {}", short_id(id))))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(ui::spinner("teams-endorse-wait", 12., theme::text_faint()))
                        .child(ui::text(Type::Caption, theme::text_faint(), "Waiting for approval…")),
                ),
        )
    }
}

impl Render for TeamsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let teams = self.teams.read(cx);
        let login = teams
            .me()
            .filter(|_| teams.enabled())
            .map(|m| crate::redact::value(&m.login));
        let hub = self.hub.read(cx);
        let device = hub.device.clone();
        let load = hub.load.clone();
        let empty = hub.teams.is_empty() && hub.invites.is_empty() && hub.asked().is_empty();
        let no_teams = hub.teams.is_empty();
        let asking = !hub.asked().is_empty();
        let error = hub.error.clone();
        let info = hub.info.clone();
        let registered = device == DeviceState::Registered;

        let header =
            div()
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
                .child(icon("message-multiple01", 14.).text_color(theme::text_subtle()))
                .child(div().flex_none().child("Teams"))
                .child(div().flex_1())
                .when_some(login.clone(), |d, l| {
                    d.child(div().text_color(theme::text_faint()).child(format!("@{l}")))
                })
                .when(registered, |d| {
                    d.child(
                        ui::icon_button("teams-refresh", "refresh01", 13.)
                            .tooltip(ui::tip("Read everything again"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.hub.update(cx, |h, cx| h.refresh(cx))
                            })),
                    )
                    .child(
                        ui::button_with(
                            "teams-devices",
                            Some("computer"),
                            "Your devices",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.hub.update(cx, |h, cx| h.load_my_devices(cx));
                            this.show(Pane::Devices, window, cx);
                        })),
                    )
                })
                .when(login.is_some(), |d| {
                    d.child(
                        ui::button("teams-sign-out", "Sign out", ButtonKind::Ghost, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.teams.update(cx, |t, cx| t.sign_out(cx));
                            }),
                        ),
                    )
                });

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

        let body: AnyElement = if login.is_none() {
            // The tab can stay open after a sign-out, or after the server took
            // the flag away.
            Self::centered(ui::empty_state(
                "user",
                "Teams is not on here",
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
                _ => {
                    let pane = match self.pane.clone() {
                        Pane::NewTeam => self.render_new_team(cx),
                        Pane::Devices => self.render_devices(cx),
                        Pane::Team(t) => self.render_team(&t, cx),
                        Pane::Channel(t, c) => self.render_channel(&t, &c, cx),
                        Pane::None if empty => Self::centered(ui::empty_state(
                            "message-multiple01",
                            "You're not in a team yet",
                            &format!(
                                "Create a team, or ask a team's admin to invite @{}. Invites show up here.",
                                login.clone().unwrap_or_default()
                            ),
                            Some(
                                ui::button_with(
                                    "teams-empty-new",
                                    Some("add01"),
                                    "New team",
                                    ButtonKind::Primary,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show(Pane::NewTeam, window, cx)
                                }))
                                .into_any_element(),
                            ),
                        )),
                        Pane::None if no_teams && asking => Self::centered(ui::empty_state(
                            "clock01",
                            "Waiting for approval",
                            "An admin approves you from one of their devices, which hands yours the team key. This page updates by itself.",
                            None,
                        )),
                        Pane::None if no_teams => Self::centered(ui::empty_state(
                            "message-multiple01",
                            "You're invited",
                            "Accept an invite on the left. An admin of the team then approves you.",
                            None,
                        )),
                        Pane::None => Self::centered(ui::empty_state(
                            "message01",
                            "No channel open",
                            "Pick a team or a channel on the left.",
                            None,
                        )),
                    };
                    let unreachable = match &load {
                        Load::Unreachable(e) => Some(e.clone()),
                        _ => None,
                    };
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .children(unreachable.map(|e| {
                            Self::banner(
                                "alert-circle",
                                Tone::Warning,
                                format!(
                                    "Can't reach the Teams server: {}",
                                    crate::redact::text(&e)
                                ),
                            )
                            .child(retry("teams-retry-banner", cx))
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
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .child(self.render_list(cx))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .min_h_0()
                                        .child(pane),
                                ),
                        )
                        .into_any_element()
                }
            }
        };

        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(header)
            .child(body)
    }
}

/// The first characters of a device ID, enough to compare by eye.
fn short_id(id: &str) -> String {
    let s: Vec<char> = id.chars().take(8).collect();
    let (a, b) = s.split_at(s.len().min(4));
    format!(
        "{} {}",
        a.iter().collect::<String>(),
        b.iter().collect::<String>()
    )
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

/// When something happened: the time today, else the day and the time.
fn when(at: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (Some(t), Some(today)) = (
        crate::platform::local_time(at),
        crate::platform::local_time(now),
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
    fn short_ids_come_in_two_groups() {
        assert_eq!(short_id("5aGHOa1mWiRjRcHwVBGPDw"), "5aGH Oa1m");
        assert_eq!(short_id("ab"), "ab ");
    }
}
