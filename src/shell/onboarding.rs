//! The setup shown before the first project, and again from the palette. It
//! takes the window's own shape: the steps where the rail is, the step in
//! view on the sheet (`crate::onboarding` holds the steps and what they look
//! for). Adding a project ends it.

use super::*;
use crate::onboarding::{self, AgentRow, GitHub, Readiness, Step};
use crate::ui::{ButtonKind, Typography as _};
use gpui::ElementId;

/// The column of steps, a little wider than the rail it stands in for.
const SIDE_W: f32 = 264.;
/// How wide a step is: a column to read, not a page.
const STEP_W: f32 = 560.;
/// The raccoon beside the steps, against the start screen's.
const FACE_SCALE: f32 = 0.5;
const FACE_W: f32 = 200.;

pub(super) struct Onboarding {
    steps: Vec<Step>,
    at: usize,
    /// No project yet: the last step adds the first one. Otherwise it was
    /// opened again from the palette, and can simply be closed.
    fresh: bool,
    /// None while the disk is searched.
    agents: Option<Vec<AgentRow>>,
    /// None while `gh` is asked.
    github: Option<GitHub>,
    /// Open the Phone tab once there is a project to hold it.
    pair_phone: bool,
    /// The last step's repository picker, made when the step first shows.
    picker: Option<(Entity<FolderPicker>, Subscription)>,
    face: Entity<crate::ascii_face::AsciiFace>,
    focus: FocusHandle,
}

impl Onboarding {
    fn step(&self) -> Step {
        self.steps[self.at]
    }
}

impl Shell {
    /// Opens the setup at launch for someone new (`onboarding::opens_at_launch`).
    pub fn onboard_if_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if onboarding::opens_at_launch(self.has_projects(), onboarding::done()) {
            self.start_onboarding(window, cx);
        }
    }

    pub(super) fn start_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.onboarding.is_some() {
            return;
        }
        self.menu = None;
        self.env_menu_open = false;
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.onboarding = Some(Onboarding {
            steps: onboarding::steps(crate::telemetry::should_ask()),
            at: 0,
            fresh: !self.has_projects(),
            agents: None,
            github: None,
            pair_phone: false,
            picker: None,
            face: cx.new(|cx| crate::ascii_face::AsciiFace::new(cx).scaled(FACE_SCALE)),
            focus,
        });
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn(async { onboarding::find_agents() })
                .await;
            let _ = this.update(cx, |this, cx| this.onboarding_found_agents(rows, cx));
        })
        .detach();
        self.onboarding_check_github(cx);
        cx.notify();
    }

    fn onboarding_found_agents(&mut self, rows: Vec<AgentRow>, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        // Someone new has picked nothing yet: start them on an agent that
        // can run here. A returning user's default is theirs.
        let pick = o
            .fresh
            .then(|| onboarding::pick(&rows, crate::model::default_agent()))
            .flatten();
        o.agents = Some(rows);
        if let Some(agent) = pick {
            self.onboarding_pick_agent(agent, cx);
        }
        cx.notify();
    }

    fn onboarding_pick_agent(&mut self, agent: crate::model::Agent, cx: &mut Context<Self>) {
        if crate::model::default_agent() != agent {
            crate::model::set_default_agent(agent);
            self.save(cx);
        }
        cx.notify();
    }

    fn onboarding_check_github(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        o.github = None;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async { onboarding::find_github() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(o) = this.onboarding.as_mut() {
                    o.github = Some(found);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn onboarding_move(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_ref() else {
            return;
        };
        let to = o.at.saturating_add_signed(by).min(o.steps.len() - 1);
        self.onboarding_go(to, window, cx);
    }

    /// Shows step `to`. The last one holds the repository picker, which
    /// takes the keys while it shows.
    fn onboarding_go(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        o.at = to.min(o.steps.len() - 1);
        if o.step() != Step::Project {
            window.focus(&o.focus, cx);
        } else if let Some((picker, _)) = &o.picker {
            let field = picker.focus_handle(cx);
            window.focus(&field, cx);
        } else {
            let existing = self.projects.iter().map(|p| p.path.clone()).collect();
            let picker = cx.new(|cx| FolderPicker::new(existing, window, cx).inline());
            let sub =
                cx.subscribe_in(
                    &picker,
                    window,
                    |this, _, ev: &PickerEvent, window, cx| match ev {
                        PickerEvent::Picked(path) => this.add_project(path.clone(), window, cx),
                        // Esc in its field: only a setup opened again closes.
                        PickerEvent::Dismissed => {
                            if this.onboarding.as_ref().is_some_and(|o| !o.fresh) {
                                this.finish_onboarding(window, cx);
                            }
                        }
                    },
                );
            if let Some(o) = self.onboarding.as_mut() {
                o.picker = Some((picker, sub));
            }
        }
        cx.notify();
    }

    /// Dark or light, applied at once and kept with the palettes.
    fn onboarding_set_light(&mut self, light: bool, cx: &mut Context<Self>) {
        let mut saved = crate::palettes::load();
        if saved.light == light {
            return;
        }
        saved.light = light;
        crate::palettes::apply(&saved);
        let version = crate::palettes::next_version();
        cx.background_spawn(async move {
            crate::palettes::store_version(&saved, version);
        })
        .detach();
        cx.refresh_windows();
        cx.notify();
    }

    /// The answer to the reports step. As in the dialog: followed for this
    /// run either way; unsaved, the question comes back next launch.
    fn onboarding_answer(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::telemetry::Consent;
        let choice = if yes { Consent::ALL } else { Consent::NONE };
        if let Err(e) = crate::telemetry::set_consent(choice) {
            eprintln!("arakun: {e}");
        }
        self.onboarding_move(1, window, cx);
    }

    /// Adds what the last step's picker has highlighted.
    fn onboarding_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((picker, _)) = self.onboarding.as_ref().and_then(|o| o.picker.as_ref()) {
            picker.clone().update(cx, |p, cx| p.add_target(window, cx));
        }
    }

    /// What Enter does on the step in view, when no field has the keys.
    fn onboarding_advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_ref() else {
            return;
        };
        match o.step() {
            Step::Project => self.onboarding_add(window, cx),
            // Its two answers are the way on, and neither is the default.
            Step::Reports if crate::telemetry::should_ask() => {}
            _ => self.onboarding_move(1, window, cx),
        }
    }

    /// Closes the setup for good: finished with a project, or skipped. The
    /// question about reports is still asked when the step was passed over.
    pub(super) fn finish_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.take() else {
            return;
        };
        let outcome = match (o.fresh, self.has_projects()) {
            (false, _) => "reopened",
            (true, true) => "project added",
            (true, false) => "skipped",
        };
        // The step it ended on shows where people who skip leave.
        crate::telemetry::capture(
            crate::telemetry::events::ONBOARDING_FINISHED,
            &[("outcome", outcome), ("step", o.step().label())],
        );
        cx.background_spawn(async { onboarding::set_done() })
            .detach();
        if self.has_projects() {
            window.focus(&self.composer_focus, cx);
            if o.pair_phone {
                self.open_phone_tab(window, cx);
            }
        } else {
            window.focus(&self.focus, cx);
        }
        self.ask_telemetry(window, cx);
        cx.notify();
    }

    /// Where the keys go while the setup shows, for a dialog that closes over it.
    pub(super) fn onboarding_focus(&self) -> Option<&FocusHandle> {
        self.onboarding.as_ref().map(|o| &o.focus)
    }

    // -----------------------------------------------------------------------
    // Drawing

    pub(super) fn render_onboarding(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let o = self.onboarding.as_ref()?;
        let step = o.step();
        let title = match step {
            Step::Project if !o.fresh => "Add another project",
            step => step.title(),
        };
        let body = match step {
            Step::Agents => self.onboarding_agents(o, cx),
            Step::Look => self.onboarding_look(cx),
            Step::Connections => self.onboarding_connections(o, cx),
            Step::Reports => self.onboarding_reports(cx),
            Step::Project => match &o.picker {
                Some((picker, _)) => picker.clone().into_any_element(),
                None => div().into_any_element(),
            },
        };
        let sheet = ui::card()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(theme::sheet())
            .child(
                div()
                    .id("onboarding-step")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .items_center()
                    .px(ui::sp(24.))
                    .pb(ui::sp(24.))
                    // The title stays at one height from step to step; in a
                    // short window this room gives way first.
                    .child(div().flex_shrink(1.).flex_basis(px(136.)).min_h(px(24.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w_full()
                            .max_w(px(STEP_W))
                            .gap(ui::sp(20.))
                            .child(ui::page_title(title, Some(step.blurb().into_any_element())))
                            .child(body),
                    ),
            )
            .child(self.onboarding_footer(o, cx));
        Some(
            div()
                .id("onboarding")
                .map(crate::ui::stop_window_move)
                .track_focus(&o.focus)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                    // The picker's field has its own Enter and Esc.
                    if !this
                        .onboarding
                        .as_ref()
                        .is_some_and(|o| o.focus.is_focused(window))
                    {
                        return;
                    }
                    match ev.keystroke.key.as_str() {
                        "enter" => this.onboarding_advance(window, cx),
                        // Only a setup opened again: the first one ends with
                        // a choice, Skip or a project.
                        "escape" if this.onboarding.as_ref().is_some_and(|o| !o.fresh) => {
                            this.finish_onboarding(window, cx)
                        }
                        _ => {}
                    }
                }))
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .gap(crate::ui::window_gap_px())
                .child(self.onboarding_side(o, cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(home::welcome_bar(window.is_maximized(), None, px(0.)))
                        .child(sheet),
                )
                .into_any_element(),
        )
    }

    /// Where the rail is in the app: the raccoon, the welcome and the steps,
    /// with the way out at the bottom, where the rail has Settings.
    fn onboarding_side(&self, o: &Onboarding, cx: &mut Context<Self>) -> gpui::Div {
        let mut steps = div().flex().flex_col().gap(ui::sp(2.));
        for (i, step) in o.steps.iter().enumerate() {
            let (now, done) = (i == o.at, i < o.at);
            let mark = div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(18.))
                .rounded_full()
                .ty(ui::Type::Caption)
                .font_family(theme::FONT_MONO);
            let mark = if done {
                mark.bg(theme::fill_hover())
                    .child(ui::icon("tick02", 12.).text_color(theme::success()))
            } else if now {
                mark.bg(theme::content())
                    .text_color(theme::background())
                    .child((i + 1).to_string())
            } else {
                mark.border_1()
                    .border_color(theme::border_strong())
                    .text_color(theme::text_subtle())
                    .child((i + 1).to_string())
            };
            let label = match step {
                Step::Project if !o.fresh => "Project",
                step => step.label(),
            };
            steps = steps.child(
                div()
                    .id(("onboarding-to", i))
                    .flex()
                    .items_center()
                    .gap(ui::sp(10.))
                    .h(px(ui::INPUT_H))
                    .px(ui::sp(10.))
                    .rounded(ui::rad(ui::RADIUS))
                    .ty(ui::Type::Body)
                    .text_color(if done {
                        theme::text_muted()
                    } else {
                        theme::text_subtle()
                    })
                    .when(now, |d| {
                        d.bg(theme::fill_selected())
                            .text_color(theme::text())
                            .font_weight(FontWeight::MEDIUM)
                    })
                    // Back to a step already seen. Ahead goes through Next,
                    // so the question about reports is not stepped over.
                    .when(done, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(theme::fill_hover()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.onboarding_go(i, window, cx)
                            }))
                    })
                    .child(mark)
                    .child(label),
            );
        }
        let leave = ui::button(
            "onboarding-skip",
            if o.fresh { "Skip setup" } else { "Close" },
            ButtonKind::Ghost,
            true,
        )
        .on_click(cx.listener(|this, _, window, cx| this.finish_onboarding(window, cx)));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDE_W))
            .px(ui::sp(8.))
            .pb(ui::sp(8.))
            // As tall as the bar beside it: the window moves by it, and the
            // traffic lights sit in it on macOS.
            .child(
                div()
                    .flex_none()
                    .h(px(ui::BAR_H))
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        crate::ui::drag_window(ev, window)
                    })
                    .when(cfg!(windows), |d| {
                        d.window_control_area(WindowControlArea::Drag)
                    }),
            )
            .child(div().flex_none().w(px(FACE_W)).child(o.face.clone()))
            .child(
                div()
                    .mt(px(8.))
                    .px(ui::sp(10.))
                    .ty(ui::Type::Title)
                    .child("Welcome to Arakun"),
            )
            .child(
                ui::text(
                    ui::Type::Body,
                    theme::text_subtle(),
                    "One native window for your coding agents.",
                )
                .mt(px(4.))
                .px(ui::sp(10.)),
            )
            .child(
                ui::overline("Setup")
                    .mt(px(32.))
                    .px(ui::sp(10.))
                    .pb(ui::sp(8.)),
            )
            .child(steps)
            .child(div().flex_1().min_h(px(16.)))
            .child(div().flex().child(leave))
    }

    /// Where the step is, then Back and the step's main button.
    fn onboarding_footer(&self, o: &Onboarding, cx: &mut Context<Self>) -> gpui::Div {
        let mut actions = div().flex().items_center().gap(ui::sp(8.));
        if o.at > 0 {
            actions = actions.child(
                ui::button("onboarding-back", "Back", ButtonKind::Ghost, true).on_click(
                    cx.listener(|this, _, window, cx| this.onboarding_move(-1, window, cx)),
                ),
            );
        }
        actions = match o.step() {
            Step::Project => {
                let target = o
                    .picker
                    .as_ref()
                    .and_then(|(p, _)| p.read(cx).target_name());
                let label = match &target {
                    Some(name) => format!("Add {name}"),
                    None => "Add project".to_string(),
                };
                // With a project already, adding one more is the side road.
                let kind = if o.fresh {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Outline
                };
                let can = target.is_some();
                let add = ui::button_with("onboarding-add", Some("folder-add"), label, kind, can)
                    .max_w(px(280.))
                    .when(can, |d| {
                        d.on_click(
                            cx.listener(|this, _, window, cx| this.onboarding_add(window, cx)),
                        )
                    });
                actions.child(add).when(!o.fresh, |d| {
                    d.child(
                        ui::button("onboarding-done", "Done", ButtonKind::Primary, true).on_click(
                            cx.listener(|this, _, window, cx| this.finish_onboarding(window, cx)),
                        ),
                    )
                })
            }
            Step::Reports if crate::telemetry::should_ask() => actions,
            _ => actions.child(
                ui::button("onboarding-next", "Next", ButtonKind::Primary, true).on_click(
                    cx.listener(|this, _, window, cx| this.onboarding_move(1, window, cx)),
                ),
            ),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(60.))
            .px(ui::sp(20.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                ui::text(
                    ui::Type::Label,
                    theme::text_faint(),
                    format!("Step {} of {}", o.at + 1, o.steps.len()),
                )
                .flex_1(),
            )
            .child(actions)
    }

    fn onboarding_agents(&self, o: &Onboarding, cx: &mut Context<Self>) -> AnyElement {
        let Some(rows) = &o.agents else {
            return ui::loading("onboarding-agents-loading", "Looking for agents")
                .into_any_element();
        };
        let picked = crate::model::default_agent();
        let mut list = ui::surface_card().p(ui::sp(4.)).gap(ui::sp(2.));
        for (i, row) in rows.iter().enumerate() {
            let agent = row.agent;
            let usable = row.state != Readiness::Missing;
            let on = usable && picked == agent;
            let says = match row.state {
                Readiness::Ready => "Ready to start a session",
                Readiness::OnFirstUse => "Installed with npm at the first session, after asking",
                Readiness::Missing => "Not found on this machine",
            };
            let end = match row.state {
                Readiness::Ready => ui::badge("Ready", ui::Tone::Success).into_any_element(),
                Readiness::OnFirstUse => {
                    ui::badge("Set up at first session", ui::Tone::Neutral).into_any_element()
                }
                Readiness::Missing => {
                    let (label, url) = onboarding::install_link(agent);
                    link(("onboarding-agent-get", i), label)
                        .on_click(|_, _, cx| cx.open_url(url))
                        .into_any_element()
                }
            };
            let line = pick_row(
                ("onboarding-agent", i),
                agent.kind().icon(16.),
                agent.label(),
                says,
                on,
            )
            .child(end);
            list = list.child(if usable {
                line.cursor_pointer()
                    .when(!on, |d| d.hover(|s| s.bg(theme::fill_hover())))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.onboarding_pick_agent(agent, cx)),
                    )
                    .child(ui::radio(on))
            } else {
                // Nothing to pick, only where to get it.
                line.text_color(theme::text_subtle())
            });
        }
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(20.))
            .child(list)
            .child(note(
                "An agent that is not signed in says how when you start a session with it.",
            ))
            .into_any_element()
    }

    fn onboarding_look(&self, cx: &mut Context<Self>) -> AnyElement {
        let light = theme::is_light();
        let mode = |id: &'static str, label: &'static str, on: bool| {
            ui::segment(id, None, label, on == light)
                .on_click(cx.listener(move |this, _, _, cx| this.onboarding_set_light(on, cx)))
        };
        const KEYS: [(&[&str], &str); 5] = [
            (&["Ctrl", "K"], "Find anything: sessions, files, commands"),
            (&["Ctrl", "T"], "New agent session"),
            (
                &["Ctrl", "⇧", "N"],
                "New environment: a worktree on a new branch",
            ),
            (&["Ctrl", "J"], "Show or hide the terminal"),
            (&["Ctrl", ","], "Settings"),
        ];
        let mut keys = ui::surface_card().py(ui::sp(4.));
        for (combo, what) in KEYS {
            keys = keys.child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(10.))
                    .min_h(px(ui::TOOLBAR_H))
                    .px(ui::sp(12.))
                    .child(ui::text(ui::Type::Body, theme::text_muted(), what).flex_1())
                    .child(ui::shortcut(combo)),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(20.))
            .child(
                ui::surface_card().child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(16.))
                        .py(ui::sp(12.))
                        .pl(ui::sp(16.))
                        .pr(ui::sp(12.))
                        .child(titled(
                            "Mode",
                            "Dark draws the palette on black, Light on white. It changes as you click.",
                        ))
                        .child(
                            ui::segmented()
                                .child(mode("onboarding-dark", "Dark", false))
                                .child(mode("onboarding-light", "Light", true)),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(8.))
                    .child(ui::overline("Five shortcuts worth knowing").pl(ui::sp(4.)))
                    .child(keys),
            )
            .into_any_element()
    }

    fn onboarding_connections(&self, o: &Onboarding, cx: &mut Context<Self>) -> AnyElement {
        let github = match o.github {
            None => ui::spinner("onboarding-gh-spin", 14., theme::text_faint()).into_any_element(),
            Some(GitHub::Connected) => ui::badge("Connected", ui::Tone::Success).into_any_element(),
            Some(found) => {
                let again = ui::button(
                    "onboarding-gh-again",
                    "Check again",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.onboarding_check_github(cx)));
                let fix = if found == GitHub::SignedOut {
                    let copied = crate::agent::was_copied(onboarding::GH_LOGIN);
                    ui::button_with(
                        "onboarding-gh-copy",
                        Some(if copied { "tick02" } else { "copy01" }),
                        onboarding::GH_LOGIN,
                        ButtonKind::Outline,
                        true,
                    )
                    .tooltip(ui::tip("Copy the command"))
                    .on_click(|_, window, cx| {
                        crate::agent::copy_with_feedback(onboarding::GH_LOGIN, window, cx)
                    })
                } else {
                    ui::button_with(
                        "onboarding-gh-get",
                        Some("link-square02"),
                        "Get the GitHub CLI",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(|_, _, cx| cx.open_url(onboarding::GH_URL))
                };
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(again)
                    .child(fix)
                    .into_any_element()
            }
        };
        let github_says = match o.github {
            Some(GitHub::SignedOut) => "Signed out. Run this in a terminal:",
            Some(GitHub::NoGh) => "The GitHub CLI is not installed",
            _ => "Pull requests, reviews and CI, through the GitHub CLI's sign-in",
        };
        let phone = if o.fresh {
            div()
                .id("onboarding-phone")
                .flex()
                .items_center()
                .gap(ui::sp(8.))
                .cursor_pointer()
                .child(ui::text(
                    ui::Type::Label,
                    theme::text_muted(),
                    "Pair after setup",
                ))
                .child(ui::switch(o.pair_phone))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(o) = this.onboarding.as_mut() {
                        o.pair_phone = !o.pair_phone;
                        cx.notify();
                    }
                }))
                .into_any_element()
        } else {
            ui::button(
                "onboarding-phone",
                "Open the Phone tab",
                ButtonKind::Outline,
                true,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(o) = this.onboarding.as_mut() {
                    o.pair_phone = true;
                }
                this.finish_onboarding(window, cx);
            }))
            .into_any_element()
        };
        let row = |id: &'static str, icon_name: &'static str, name, says, end: AnyElement| {
            let lead = ui::icon(icon_name, 16.).text_color(theme::text_muted());
            div()
                .p(ui::sp(4.))
                .child(pick_row(id, lead.into_any_element(), name, says, false).child(end))
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(20.))
            .child(
                ui::surface_card()
                    .child(row(
                        "onboarding-gh",
                        "github",
                        "GitHub",
                        github_says,
                        github,
                    ))
                    .child(div().h(px(1.)).bg(theme::border_subtle()))
                    .child(row(
                        "onboarding-phone-row",
                        "signal-full02",
                        "Phone remote",
                        "Follow sessions and answer agents from your phone",
                        phone,
                    )),
            )
            .when(o.fresh, |d| {
                d.child(note(
                    "Pairing needs a project, so the Phone tab opens once you have added one.",
                ))
            })
            .into_any_element()
    }

    fn onboarding_reports(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::telemetry_dialog::{DETAILS_URL, POINTS, point};
        // Equal width and the same kind: neither answer is the easy one.
        let answer = |id: &'static str, label: &'static str, yes: bool| {
            ui::button(id, label, ButtonKind::Outline, true)
                .flex_1()
                .on_click(
                    cx.listener(move |this, _, window, cx| this.onboarding_answer(yes, window, cx)),
                )
        };
        let said = match crate::telemetry::consent() {
            Some(c) if c == crate::telemetry::Consent::NONE => {
                "You chose not to send them. Change it any time in Settings › Privacy."
            }
            Some(_) => "You chose to send them. Change it any time in Settings › Privacy.",
            None => "Either answer moves on. Change it any time in Settings › Privacy.",
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(20.))
            .child(
                ui::surface_card()
                    .gap(ui::sp(12.))
                    .p(ui::sp(16.))
                    .ty(ui::Type::Body)
                    .text_color(theme::text_muted())
                    .children(POINTS.map(|(icon_name, text)| point(icon_name, text)))
                    .child(
                        // In line with the sentences, past their icons.
                        div().flex().pl(ui::sp(24.)).child(
                            link("onboarding-reports-details", "What is sent")
                                .on_click(|_, _, cx| cx.open_url(DETAILS_URL)),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(ui::sp(8.))
                    .child(answer("onboarding-reports-no", "Don't send", false))
                    .child(answer("onboarding-reports-yes", "Send reports", true)),
            )
            .child(note(said).text_center())
            .into_any_element()
    }
}

/// A name with a line under it, at the start of a row.
fn titled(name: &'static str, says: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(ui::sp(3.))
        .child(
            ui::text(ui::Type::Body, theme::text(), name)
                .font_weight(FontWeight::MEDIUM)
                .truncate(),
        )
        .child(ui::text(ui::Type::Caption, theme::text_subtle(), says))
}

/// A row of a step's card, as the Add project list draws its own: something
/// in a tile, a name with a line under it, and room at the end for the
/// caller's state or control.
fn pick_row(
    id: impl Into<ElementId>,
    lead: AnyElement,
    name: &'static str,
    says: &'static str,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(ui::sp(12.))
        .min_h(px(44.))
        .px(ui::sp(10.))
        .rounded(ui::rad(ui::RADIUS))
        .when(on, |d| d.bg(theme::fill_selected()))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(ui::BUTTON_H))
                .rounded(ui::rad(ui::RADIUS))
                .bg(theme::fill_hover())
                .child(lead),
        )
        .child(titled(name, says))
}

/// A quiet line under a step's card.
fn note(content: impl Into<SharedString>) -> gpui::Div {
    ui::text(ui::Type::Caption, theme::text_subtle(), content)
}

/// Text that opens a page outside the app.
fn link(id: impl Into<ElementId>, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(ui::sp(4.))
        .ty(ui::Type::Label)
        .text_color(theme::accent())
        .cursor_pointer()
        .hover(|s| s.underline())
        .child(label)
        .child(ui::icon("link-square02", 12.).text_color(theme::accent()))
}
