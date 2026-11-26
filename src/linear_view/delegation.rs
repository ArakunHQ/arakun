//! The Linear tab's page for starting sessions from Linear: the switch that
//! lets Linear send issues to this computer, and the mappings from a Linear
//! team or project to a project here. The page only saves the choice; the
//! shell tells Arakun.

use std::path::PathBuf;

use super::*;
use crate::linear::delegation::{Kind, Mapping};
use crate::linear::places::Place;

/// Why the switch cannot do its job yet, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    SignIn,
    Relay,
    NotInstalled,
}

/// The first thing in the way. `installed` None is not known yet, which is
/// not a blocker.
pub fn blocker(host: &Host, installed: Option<bool>) -> Option<Blocker> {
    if !host.signed_in {
        Some(Blocker::SignIn)
    } else if !host.relay_on {
        Some(Blocker::Relay)
    } else if installed == Some(false) {
        Some(Blocker::NotInstalled)
    } else {
        None
    }
}

/// Linking works before the app is installed, so only the account and the
/// relay are needed.
pub fn can_turn_on(host: &Host) -> bool {
    host.signed_in && host.relay_on
}

/// What shows under the switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Notes {
    /// The help text, which says the Linear key is sent.
    pub help: bool,
    pub blocker: Option<Blocker>,
    pub sync: bool,
    /// The card that asks before the key of the workspace in view is sent.
    pub connect: bool,
}

/// While off, the help always shows: turning the switch on sends the Linear
/// key, so that must be on screen first, blocker or not. While on, a
/// workspace the person has not agreed for asks first, unless the server
/// has a link for it already (`installed` is known): then no key goes.
/// The sync line shows only once a sync can run and has.
pub(super) fn notes(
    host: &Host,
    installed: Option<bool>,
    on: bool,
    consented: bool,
    sync: Sync,
) -> Notes {
    let connect = on && !consented && installed.is_none();
    Notes {
        help: !on,
        blocker: blocker(host, installed),
        sync: on && !connect && host.signed_in && host.relay_on && sync != Sync::Unknown,
        connect,
    }
}

/// Under the switch while it is off. It names the workspace whose key goes.
fn help_text(workspace: &str) -> String {
    format!(
        "When you assign an issue to Arakun in Linear, a session starts in the project you map. Turning this on sends your Linear key for {workspace} to Arakun, to prove the Linear user is you. It is not stored there. A project on one of your servers runs there, and that server keeps the key to read its issues."
    )
}

/// Under the switch while it is on: how to reach a session from Linear.
const TALK_TEXT: &str = "Reply in the issue to send its session a message. It waits for the current turn, unless it starts with ! (or /interrupt): then the turn stops and the session works on your message at once. Stop in Linear ends the session's work.";

fn connect_text(workspace: &str) -> String {
    format!(
        "{workspace} is not connected to Arakun yet. Connecting sends your Linear key for {workspace} to Arakun, to prove the Linear user is you. It is not stored there."
    )
}

/// The kind, the Linear name, and the project's name, or None when the
/// project is no longer in Arakun.
pub(super) fn row_text(m: &Mapping, projects: &[Target]) -> (&'static str, String, Option<String>) {
    (
        kind_name(m.kind),
        m.name.clone(),
        project_name(projects, m).map(str::to_string),
    )
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Team => "Team",
        Kind::Project => "Project",
    }
}

fn project_name<'a>(projects: &'a [Target], m: &Mapping) -> Option<&'a str> {
    projects
        .iter()
        .find(|t| t.is(&m.project, m.server.as_deref()))
        .map(|t| t.name.as_str())
}

/// Under a row whose project is not in the list.
fn gone_text(m: &Mapping) -> &'static str {
    if m.server.is_some() {
        "This project is not in Arakun now. Its server may not be connected."
    } else {
        "This project is no longer in Arakun."
    }
}

/// What the add menu offers in `workspace`: teams, then projects, leaving
/// out what is mapped there already.
pub(super) fn choices<'a>(
    places: &'a Places,
    mappings: &[Mapping],
    workspace: &str,
) -> Vec<(Kind, &'a Place)> {
    let mapped = |kind: Kind, id: &str| {
        mappings
            .iter()
            .any(|m| m.workspace == workspace && m.kind == kind && m.target == id)
    };
    let teams = places.teams.iter().map(|p| (Kind::Team, p));
    let projects = places.projects.iter().map(|p| (Kind::Project, p));
    teams
        .chain(projects)
        .filter(|(k, p)| !mapped(*k, &p.id))
        .collect()
}

pub(super) fn sync_line(sync: Sync) -> &'static str {
    match sync {
        // `notes` draws no line for it.
        Sync::Unknown => "",
        Sync::Idle => "Up to date.",
        Sync::Sending => "Sending…",
        Sync::Failed => {
            "Couldn't update Arakun. It is tried again when this computer reconnects or you change something here."
        }
        Sync::Limited => {
            "Too many requests. It is tried again when this computer reconnects or you change something here."
        }
    }
}

/// The add menu's two steps: the Linear team or project, then the project here.
pub(super) enum Adding {
    Target,
    Project {
        kind: Kind,
        target: String,
        name: String,
    },
}

impl LinearView {
    /// The Linear organization id of the workspace in view.
    fn workspace_id(&self) -> Option<String> {
        self.session().map(|s| s.workspace_id.clone())
    }

    /// The name of the workspace in view, as the page shows it.
    fn workspace_name(&self) -> String {
        self.session()
            .map(|s| crate::redact::value(&s.workspace).to_string())
            .unwrap_or_default()
    }

    /// Reads the teams and projects of the workspace in view, unless they
    /// are here already. An answer for another workspace is dropped.
    pub(super) fn read_places(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session().cloned() else {
            return;
        };
        if self.places.loading() || (self.places.data.is_some() && self.places.error.is_none()) {
            return;
        }
        let until = self.until(&s.id);
        if crate::model::unix_now() < until {
            self.places.error = Some(ApiError::Limited { until });
            cx.notify();
            return;
        }
        let id = s.id.clone();
        self.places.start(
            cx,
            |this| &mut this.places,
            async move { linear::places::places(&s) },
            move |this, got, _| {
                if this.current_id().as_deref() != Some(id.as_str()) {
                    return;
                }
                this.note_limit(&id, &got);
                this.places.settle(got);
            },
        );
    }

    /// Saves a changed `Delegation` before telling the shell. A failed save
    /// changes nothing and says why.
    fn change_delegation(&mut self, f: impl FnOnce(&mut Delegation), cx: &mut Context<Self>) {
        let mut d = self.delegation.clone();
        f(&mut d);
        if d == self.delegation {
            return;
        }
        match d.save() {
            Ok(()) => {
                self.delegation = d;
                self.delegation_error = None;
                cx.emit(LinearEvent::DelegationChanged);
            }
            Err(e) => self.delegation_error = Some(e),
        }
        cx.notify();
    }

    /// Turning it on is the person's consent for the workspace in view,
    /// whose name the help text gave. Turning it off keeps the consent.
    fn toggle_delegation(&mut self, cx: &mut Context<Self>) {
        let on = self.delegation.enabled;
        // It can always be turned off.
        if !on && !can_turn_on(&self.host) {
            return;
        }
        let workspace = self.workspace_id();
        self.change_delegation(
            |d| {
                d.enabled = !on;
                if let Some(w) = workspace.filter(|_| !on) {
                    d.consent(&w);
                }
            },
            cx,
        );
    }

    /// The connect card's button: consent for the workspace in view.
    fn connect_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace_id() else {
            return;
        };
        self.change_delegation(
            |d| {
                d.consent(&workspace);
            },
            cx,
        );
    }

    fn add_mapping(
        &mut self,
        kind: Kind,
        target: String,
        name: String,
        project: PathBuf,
        server: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.adding = None;
        cx.notify();
        let Some(workspace) = self.workspace_id() else {
            return;
        };
        self.change_delegation(
            |d| {
                d.add(Mapping {
                    workspace,
                    kind,
                    target,
                    name,
                    project,
                    server,
                });
            },
            cx,
        );
    }

    fn remove_mapping(&mut self, kind: Kind, target: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace_id() else {
            return;
        };
        self.change_delegation(|d| d.remove(&workspace, kind, &target), cx);
    }

    pub(super) fn render_delegation(&self, cx: &mut Context<Self>) -> AnyElement {
        let on = self.delegation.enabled;
        let enabled = on || can_turn_on(&self.host);
        let workspace = self
            .session()
            .map(|s| s.workspace_id.clone())
            .unwrap_or_default();
        let installed = self.installed.get(&workspace).copied();
        let name = self.workspace_name();
        let switch = ui::labelled(
            div().id("lin-deleg-switch"),
            Role::Switch,
            "Let Linear start sessions on this computer",
        )
        .aria_toggled(ui::toggled(on))
        .child(ui::switch(on))
        .when(!enabled, |d| d.opacity(0.4))
        .when(enabled, |d| {
            d.cursor_pointer()
                .map(ui::focusable)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_delegation(cx)))
        });
        let notes = notes(
            &self.host,
            installed,
            on,
            self.delegation.consents(&workspace),
            self.sync,
        );
        let blocker = notes.blocker.map(|b| self.render_blocker(b, cx));
        let connect = notes.connect.then(|| self.render_connect(&name, cx));
        let card = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .w_full()
            .max_w(px(720.))
            .p(crate::ui::sp(20.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .child(
                        ui::text(
                            Type::Body,
                            theme::text(),
                            "Let Linear start sessions on this computer",
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    .child(switch),
            )
            .when(notes.help, |d| {
                d.child(ui::text(Type::Body, theme::text_muted(), help_text(&name)))
            })
            .when(on, |d| {
                d.child(ui::text(Type::Body, theme::text_muted(), TALK_TEXT))
            })
            .children(blocker)
            .children(connect)
            .when(notes.sync, |d| {
                let color = match self.sync {
                    Sync::Failed | Sync::Limited => theme::attention().into(),
                    Sync::Unknown | Sync::Idle | Sync::Sending => theme::text_faint(),
                };
                d.child(ui::text(Type::Caption, color, sync_line(self.sync)))
            })
            .when_some(self.delegation_error.as_ref(), |d, e| {
                d.child(ui::text(Type::Body, theme::error().into(), e.clone()))
            });

        let rows: Vec<&Mapping> = self
            .delegation
            .mappings
            .iter()
            .filter(|m| m.workspace == workspace)
            .collect();
        let mut list = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .max_w(px(720.))
            .child(ui::heading("Mappings"));
        if rows.is_empty() {
            list = list.child(ui::text(
                Type::Body,
                theme::text_muted(),
                "No mappings yet. Map a Linear team or project to a project on this computer.",
            ));
        }
        for (i, m) in rows.into_iter().enumerate() {
            list = list.child(self.render_mapping(i, m, cx));
        }
        let list = list.child(
            div().flex().pt(crate::ui::sp(4.)).child(
                ui::button_with(
                    "lin-deleg-add",
                    Some("add01"),
                    "Add a mapping",
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.menu = None;
                    this.adding = if this.adding.is_some() {
                        None
                    } else {
                        Some((
                            ev.position() + gpui::point(px(-8.), px(14.)),
                            Adding::Target,
                        ))
                    };
                    this.read_places(cx);
                    cx.notify();
                })),
            ),
        );

        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(6.))
                    .child(ui::heading("Start sessions from Linear")),
            )
            .child(card)
            .child(list)
            .into_any_element()
    }

    /// Asks before the Linear key of `name`, the workspace in view, is sent.
    fn render_connect(&self, name: &str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .child(
                ui::text(Type::Body, theme::text_muted(), connect_text(name))
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                ui::button(
                    "lin-deleg-connect",
                    format!("Connect {name}"),
                    ButtonKind::Primary,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.connect_workspace(cx))),
            )
            .into_any_element()
    }

    fn render_blocker(&self, b: Blocker, cx: &mut Context<Self>) -> AnyElement {
        let (line, button) = match b {
            Blocker::SignIn => (
                "Sign in to Arakun first.".to_string(),
                ui::button("lin-deleg-sign-in", "Sign in", ButtonKind::Outline, true).on_click(
                    cx.listener(|_, _, _, cx| cx.emit(LinearEvent::SignInToArakun)),
                ),
            ),
            Blocker::Relay => (
                "Turn on \"Reach this computer from anywhere\" in the Phone tab. Linear reaches this computer through it.".to_string(),
                ui::button("lin-deleg-phone", "Open the Phone tab", ButtonKind::Outline, true)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LinearEvent::OpenPhoneTab))),
            ),
            Blocker::NotInstalled => {
                let name = self
                    .session()
                    .map(|s| crate::redact::value(&s.workspace))
                    .unwrap_or_default();
                let url = self.host.install_url.clone();
                (
                    format!("Arakun is not installed in {name} yet. A Linear admin can install it."),
                    ui::button(
                        "lin-deleg-install",
                        "Open the install page",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(LinearEvent::Open(url.clone()))
                    })),
                )
            }
        };
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .child(
                ui::text(Type::Body, theme::attention().into(), line)
                    .flex_1()
                    .min_w_0(),
            )
            .child(button)
            .into_any_element()
    }

    fn render_mapping(&self, i: usize, m: &Mapping, cx: &mut Context<Self>) -> AnyElement {
        let (kind, name, project) = row_text(m, &self.host.projects);
        let name_label = name.clone();
        let broken = project.is_none();
        let (strong, soft) = if broken {
            (theme::text_faint(), theme::text_faint())
        } else {
            (theme::text(), theme::text_subtle())
        };
        let mut line = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .min_w_0()
            .child(ui::text(Type::Label, soft, kind))
            .child(ui::text(Type::Body, strong, name).truncate());
        if let Some(project) = project {
            line = line
                .child(ui::text(Type::Body, soft, "→"))
                .child(ui::text(Type::Body, strong, project).truncate());
        }
        let (k, target) = (m.kind, m.target.clone());
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(2.))
                    .child(line)
                    .when(broken, |d| {
                        d.child(ui::text(Type::Caption, theme::text_faint(), gone_text(m)))
                    }),
            )
            .child(
                ui::button(("lin-deleg-remove", i), "Remove", ButtonKind::Ghost, true)
                    // Every row has one, so the name says which.
                    .aria_label(gpui::SharedString::from(format!(
                        "Remove {kind} {name_label}"
                    )))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_mapping(k, target.clone(), cx)
                    })),
            )
            .into_any_element()
    }

    /// The add menu: a Linear team or project, then a project here.
    pub(super) fn render_add_menu(&self, step: &Adding, cx: &mut Context<Self>) -> AnyElement {
        let panel = ui::menu_panel("lin-add-menu", 300.);
        let note = |line: &str| {
            ui::text(Type::Body, theme::text_subtle(), line.to_string())
                .px(crate::ui::sp(8.))
                .py(crate::ui::sp(6.))
        };
        match step {
            Adding::Target => {
                let panel = panel.child(ui::menu_heading("Linear team or project"));
                let Some(places) = self.places.data.as_ref() else {
                    if let Some(e) = self.places.error.as_ref() {
                        let retry = matches!(e, ApiError::Failed(_)).then(|| {
                            ui::button("lin-add-retry", "Try again", ButtonKind::Outline, true)
                                .on_click(cx.listener(|this, _, _, cx| this.read_places(cx)))
                        });
                        return panel
                            .child(
                                ui::text(
                                    Type::Body,
                                    theme::error().into(),
                                    crate::redact::text(&error_line(e, crate::model::unix_now()))
                                        .into_owned(),
                                )
                                .px(crate::ui::sp(8.))
                                .py(crate::ui::sp(6.)),
                            )
                            .children(retry.map(|b| {
                                div().px(crate::ui::sp(8.)).pb(crate::ui::sp(6.)).child(b)
                            }))
                            .into_any_element();
                    }
                    return panel
                        .child(note("Reading teams and projects…"))
                        .into_any_element();
                };
                let workspace = self.workspace_id().unwrap_or_default();
                let found = choices(places, &self.delegation.mappings, &workspace);
                if found.is_empty() {
                    return panel
                        .child(note("Every team and project is mapped already."))
                        .into_any_element();
                }
                let rows = found.into_iter().enumerate().map(|(i, (kind, p))| {
                    let (target, name) = (p.id.clone(), p.name.clone());
                    ui::menu_item(
                        ("lin-add-target", i),
                        match kind {
                            Kind::Team => "user",
                            Kind::Project => "folder01",
                        },
                        format!("{}  {}", kind_name(kind), p.name),
                        false,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some((_, step)) = this.adding.as_mut() {
                            *step = Adding::Project {
                                kind,
                                target: target.clone(),
                                name: name.clone(),
                            };
                        }
                        cx.notify();
                    }))
                });
                panel
                    .child(
                        div()
                            .id("lin-add-targets")
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(1.))
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .children(rows),
                    )
                    .into_any_element()
            }
            Adding::Project { kind, target, name } => {
                let panel = panel.child(ui::menu_heading("Project"));
                if self.host.projects.is_empty() {
                    return panel
                        .child(note("Add a project to Arakun first."))
                        .into_any_element();
                }
                let rows = self.host.projects.iter().enumerate().map(|(i, t)| {
                    let (kind, target, name) = (*kind, target.clone(), name.clone());
                    let (root, server) = (t.root.clone(), t.server.clone());
                    let icon = if server.is_some() {
                        "server"
                    } else {
                        "folder01"
                    };
                    ui::menu_item(("lin-add-project", i), icon, t.name.clone(), false, true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_mapping(
                                kind,
                                target.clone(),
                                name.clone(),
                                root.clone(),
                                server.clone(),
                                cx,
                            )
                        }))
                });
                panel
                    .child(
                        div()
                            .id("lin-add-projects")
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(1.))
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .children(rows),
                    )
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::linear::delegation::{Kind, Mapping};

    fn ok() -> Host {
        Host {
            signed_in: true,
            flagged: true,
            relay_on: true,
            projects: vec![],
            install_url: String::new(),
        }
    }

    #[test]
    fn blockers_come_in_order() {
        let ok = ok();
        assert_eq!(
            blocker(
                &Host {
                    signed_in: false,
                    ..ok.clone()
                },
                Some(false)
            ),
            Some(Blocker::SignIn)
        );
        assert_eq!(
            blocker(
                &Host {
                    relay_on: false,
                    ..ok.clone()
                },
                Some(false)
            ),
            Some(Blocker::Relay)
        );
        assert_eq!(blocker(&ok, Some(false)), Some(Blocker::NotInstalled));
        assert_eq!(blocker(&ok, None), None);
        assert_eq!(blocker(&ok, Some(true)), None);
    }

    #[test]
    fn the_switch_needs_an_account_and_the_relay_but_not_the_install() {
        let ok = ok();
        assert!(!can_turn_on(&Host {
            signed_in: false,
            ..ok.clone()
        }));
        assert!(!can_turn_on(&Host {
            relay_on: false,
            ..ok.clone()
        }));
        // Not installed yet: linking still works, so it does not block.
        assert!(can_turn_on(&ok));
    }

    fn mapping(kind: Kind, target: &str, name: &str, project: &str) -> Mapping {
        Mapping {
            workspace: "w1".into(),
            kind,
            target: target.into(),
            name: name.into(),
            project: PathBuf::from(project),
            server: None,
        }
    }

    #[test]
    fn the_help_shows_whenever_the_switch_is_off() {
        let ok = ok();
        let states = [
            (
                Host {
                    signed_in: false,
                    ..ok.clone()
                },
                Some(false),
            ),
            (
                Host {
                    relay_on: false,
                    ..ok.clone()
                },
                None,
            ),
            (ok.clone(), Some(false)), // not installed, and the switch still works
            (ok.clone(), Some(true)),
            (ok.clone(), None),
        ];
        for (host, installed) in states {
            for consented in [false, true] {
                let off = notes(&host, installed, false, consented, Sync::Idle);
                assert!(
                    off.help && !off.sync && !off.connect,
                    "{host:?} {installed:?}"
                );
                assert_eq!(off.blocker, blocker(&host, installed));
                let on = notes(&host, installed, true, true, Sync::Idle);
                assert!(!on.help && !on.connect, "{host:?} {installed:?}");
                assert_eq!(on.blocker, blocker(&host, installed));
            }
        }
    }

    #[test]
    fn an_unconsented_workspace_asks_before_its_key_goes() {
        let ok = ok();
        let card =
            |installed, consented| notes(&ok, installed, true, consented, Sync::Idle).connect;
        assert!(!card(None, true)); // agreed already
        assert!(card(None, false));
        // The server has a link for it: no key goes, so nothing to ask.
        assert!(!card(Some(true), false));
        assert!(!card(Some(false), false));
        // The card stands in place of the sync line.
        assert!(!notes(&ok, None, true, false, Sync::Idle).sync);
    }

    #[test]
    fn the_sync_line_shows_only_once_a_sync_can_run_and_has() {
        let ok = ok();
        let line = |host: &Host, on, sync| notes(host, Some(true), on, true, sync).sync;
        assert!(line(&ok, true, Sync::Idle));
        assert!(line(&ok, true, Sync::Failed));
        assert!(!line(&ok, true, Sync::Unknown)); // nothing has run
        assert!(!line(&ok, false, Sync::Idle)); // off
        let signed_out = Host {
            signed_in: false,
            ..ok.clone()
        };
        let no_relay = Host {
            relay_on: false,
            ..ok.clone()
        };
        assert!(!line(&signed_out, true, Sync::Idle));
        assert!(!line(&no_relay, true, Sync::Idle));
    }

    #[test]
    fn the_texts_name_the_workspace_whose_key_goes() {
        assert_eq!(
            help_text("Acme"),
            "When you assign an issue to Arakun in Linear, a session starts in the project you map. Turning this on sends your Linear key for Acme to Arakun, to prove the Linear user is you. It is not stored there. A project on one of your servers runs there, and that server keeps the key to read its issues."
        );
        assert_eq!(
            connect_text("Acme"),
            "Acme is not connected to Arakun yet. Connecting sends your Linear key for Acme to Arakun, to prove the Linear user is you. It is not stored there."
        );
    }

    #[test]
    fn rows_name_the_project_or_say_it_is_gone() {
        let projects = vec![
            Target {
                name: "arakun".into(),
                root: PathBuf::from("/p/arakun"),
                server: None,
            },
            Target {
                name: "arakun on box".into(),
                root: PathBuf::from("/srv/arakun"),
                server: Some("s1".into()),
            },
        ];
        // The same folder on another machine is another project.
        let mut on_box = mapping(Kind::Team, "t1", "Eng", "/srv/arakun");
        assert_eq!(row_text(&on_box, &projects).2, None);
        on_box.server = Some("s1".into());
        assert_eq!(
            row_text(&on_box, &projects).2.as_deref(),
            Some("arakun on box")
        );
        assert_ne!(
            gone_text(&on_box),
            gone_text(&mapping(Kind::Team, "t1", "Eng", "/x"))
        );
        assert_eq!(
            row_text(&mapping(Kind::Team, "t1", "Eng", "/p/arakun"), &projects),
            ("Team", "Eng".to_string(), Some("arakun".to_string()))
        );
        assert_eq!(
            row_text(
                &mapping(Kind::Project, "x1", "Launch", "/p/gone"),
                &projects
            ),
            ("Project", "Launch".to_string(), None)
        );
    }

    #[test]
    fn the_menu_offers_teams_then_projects_not_yet_mapped_here() {
        let place = |id: &str, name: &str| Place {
            id: id.into(),
            name: name.into(),
        };
        let places = Places {
            teams: vec![place("t1", "App"), place("t2", "Web")],
            projects: vec![place("x1", "Launch")],
        };
        let mut mapped = vec![mapping(Kind::Team, "t1", "App", "/p/a")];
        // The same team mapped in another workspace does not count here.
        mapped.push(Mapping {
            workspace: "w2".into(),
            ..mapping(Kind::Team, "t2", "Web", "/p/a")
        });
        let got: Vec<(Kind, String)> = choices(&places, &mapped, "w1")
            .into_iter()
            .map(|(k, p)| (k, p.id.clone()))
            .collect();
        assert_eq!(
            got,
            vec![(Kind::Team, "t2".into()), (Kind::Project, "x1".into())]
        );
    }

    #[test]
    fn the_sync_line_reads_plainly() {
        assert_eq!(sync_line(Sync::Idle), "Up to date.");
        assert_eq!(sync_line(Sync::Sending), "Sending…");
        assert_eq!(
            sync_line(Sync::Failed),
            "Couldn't update Arakun. It is tried again when this computer reconnects or you change something here."
        );
        assert_eq!(
            sync_line(Sync::Limited),
            "Too many requests. It is tried again when this computer reconnects or you change something here."
        );
    }
}
