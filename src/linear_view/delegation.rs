//! The Linear tab's page for starting sessions from Linear: the switch that
//! lets Linear send issues to this computer, and the mappings from a Linear
//! team or project to a project here. The page only saves the choice; the
//! shell tells Arakun.

use std::path::{Path, PathBuf};

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

/// The kind, the Linear name, and the project's name, or None when the
/// project is no longer in Arakun.
pub(super) fn row_text(
    m: &Mapping,
    projects: &[(String, PathBuf)],
) -> (&'static str, String, Option<String>) {
    (
        kind_name(m.kind),
        m.name.clone(),
        project_name(projects, &m.project).map(str::to_string),
    )
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Team => "Team",
        Kind::Project => "Project",
    }
}

fn project_name<'a>(projects: &'a [(String, PathBuf)], root: &Path) -> Option<&'a str> {
    projects
        .iter()
        .find(|(_, p)| p == root)
        .map(|(n, _)| n.as_str())
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
        Sync::Idle => "Up to date.",
        Sync::Sending => "Sending…",
        Sync::Failed => "Couldn't reach Arakun. It is tried again when this computer reconnects.",
        Sync::Limited => "Too many requests. It is tried again in a moment.",
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

    fn toggle_delegation(&mut self, cx: &mut Context<Self>) {
        let on = self.delegation.enabled;
        // It can always be turned off.
        if !on && !can_turn_on(&self.host) {
            return;
        }
        self.change_delegation(|d| d.enabled = !on, cx);
    }

    fn add_mapping(
        &mut self,
        kind: Kind,
        target: String,
        name: String,
        project: PathBuf,
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
        let note: Option<AnyElement> = match blocker(&self.host, installed) {
            Some(b) => Some(self.render_blocker(b, cx)),
            None if !on => Some(
                ui::text(
                    Type::Body,
                    theme::text_muted(),
                    "When you assign an issue to Arakun in Linear, a session starts here in the project you map. Turning this on sends your Linear key to Arakun once, to prove the Linear user is you. It is not stored there.",
                )
                .into_any_element(),
            ),
            None => None,
        };
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
            .children(note)
            .when(on, |d| {
                let color = match self.sync {
                    Sync::Failed | Sync::Limited => theme::attention().into(),
                    Sync::Idle | Sync::Sending => theme::text_faint(),
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
            .child(ui::heading("Start sessions from Linear"))
            .child(card)
            .child(list)
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
                        d.child(ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            "This project is no longer in Arakun.",
                        ))
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
                let panel = panel.child(ui::menu_heading("Project on this computer"));
                if self.host.projects.is_empty() {
                    return panel
                        .child(note("Add a project to Arakun first."))
                        .into_any_element();
                }
                let rows = self
                    .host
                    .projects
                    .iter()
                    .enumerate()
                    .map(|(i, (pname, root))| {
                        let (kind, target, name, root) =
                            (*kind, target.clone(), name.clone(), root.clone());
                        ui::menu_item(
                            ("lin-add-project", i),
                            "folder01",
                            pname.clone(),
                            false,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_mapping(kind, target.clone(), name.clone(), root.clone(), cx)
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
        }
    }

    #[test]
    fn rows_name_the_project_or_say_it_is_gone() {
        let projects = vec![("arakun".to_string(), PathBuf::from("/p/arakun"))];
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
            "Couldn't reach Arakun. It is tried again when this computer reconnects."
        );
        assert_eq!(
            sync_line(Sync::Limited),
            "Too many requests. It is tried again in a moment."
        );
    }
}
