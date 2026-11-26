//! The setup shown before the first project, and again from the palette.
//! Someone new first sees the raccoon while Arakun checks the machine; then
//! four steps sit under a bar along the top: the questions on the left, and
//! on the right the window itself, small and live, changing as they pick
//! (`crate::onboarding` holds the steps and what they look for).

use super::*;
use crate::onboarding::{self, AgentRow, GitHub, Readiness, Step};
use crate::sidebar::Preset;
use crate::ui::{ButtonKind, Typography as _};
use gpui::ElementId;

/// How wide the questions are: a column to read, not a page.
const FORM_W: f32 = 540.;
/// Below this window width the live preview gives its room to the questions.
const PREVIEW_MIN_W: f32 = 1080.;
/// The raccoon on the welcome, against the start screen's.
const FACE_SCALE: f32 = 0.6;
const FACE_W: f32 = 240.;
/// The welcome's column.
const WELCOME_W: f32 = 420.;
/// The step bar holds each step's answer from this window width, and its
/// own name from `BAR_TITLE_MIN_W`; below, they give way to the steps.
const BAR_ANSWERS_MIN_W: f32 = 1280.;
const BAR_TITLE_MIN_W: f32 = 1000.;
/// The longest an answer in the step bar gets before it is cut.
const BAR_ANSWER_MAX_W: f32 = 160.;
/// Palettes and sidebar layouts per row on the Look step.
const PALETTES_PER_ROW: usize = 4;
const PRESETS_PER_ROW: usize = 3;

pub(super) struct Onboarding {
    steps: Vec<Step>,
    at: usize,
    /// No project yet when it opened. Otherwise it was opened again from the
    /// palette, and can simply be closed.
    fresh: bool,
    /// None while the disk is searched.
    agents: Option<Vec<AgentRow>>,
    /// None while `gh` is asked.
    github: Option<GitHub>,
    /// Open the Phone tab once there is a project to hold it.
    pair_phone: bool,
    /// The crash reports switch on Connect.
    reports: bool,
    /// The colors as picked here, kept so quick picks build on each other
    /// while earlier ones are still being saved, and so drawing reads no file.
    look: crate::palettes::Saved,
    /// Connect has shown, so its switch is the answer about reports.
    reports_seen: bool,
    /// The Project step's repository picker, made when the step first shows.
    picker: Option<(Entity<FolderPicker>, Subscription)>,
    face: Entity<crate::ascii_face::AsciiFace>,
    focus: FocusHandle,
}

impl Onboarding {
    fn step(&self) -> Step {
        self.steps[self.at]
    }

    /// The steps the bar counts: all but the welcome.
    fn questions(&self) -> impl Iterator<Item = (usize, Step)> + '_ {
        self.steps
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, s)| *s != Step::Welcome)
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
        self.setup_tips = false;
        // An earlier "Pair after setup" still waiting for a project shows as
        // this setup's answer, which then replaces it.
        let pair_phone = std::mem::take(&mut self.phone_after_project);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let fresh = !self.has_projects();
        let reports =
            crate::telemetry::consent().is_some_and(|c| c != crate::telemetry::Consent::NONE);
        self.onboarding = Some(Onboarding {
            steps: onboarding::steps(fresh),
            at: 0,
            fresh,
            agents: None,
            github: None,
            pair_phone,
            reports,
            look: crate::palettes::load(),
            reports_seen: false,
            picker: None,
            face: cx.new(|cx| crate::ascii_face::AsciiFace::new(cx).scaled(FACE_SCALE)),
            focus,
        });
        self.onboarding_check_agents(cx);
        self.onboarding_check_github(cx);
        cx.notify();
    }

    fn onboarding_check_agents(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        o.agents = None;
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn(crate::guard::task(async { onboarding::find_agents() }))
                .await;
            let _ = this.update(cx, |this, cx| this.onboarding_found_agents(rows, cx));
        })
        .detach();
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

    fn onboarding_pick_agent(&mut self, agent: AgentKind, cx: &mut Context<Self>) {
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
                .background_spawn(crate::guard::task_or(
                    async { onboarding::find_github() },
                    // The step then offers to install it, which also checks again.
                    |_| onboarding::GitHub::NoGh,
                ))
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

    /// Shows step `to`. The Project step holds the repository picker, which
    /// takes the keys while it shows.
    fn onboarding_go(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        o.at = to.min(o.steps.len() - 1);
        if o.step() == Step::Connect {
            o.reports_seen = true;
        }
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
                        // Esc in its field steps back, as it does on the other steps.
                        PickerEvent::Dismissed => this.onboarding_back(window, cx),
                        // The setup's picker is given no servers to offer.
                        PickerEvent::CloneOnServer { .. } => {}
                    },
                );
            if let Some(o) = self.onboarding.as_mut() {
                o.picker = Some((picker, sub));
            }
        }
        cx.notify();
    }

    /// Esc: the step before, or out of a setup opened again.
    fn onboarding_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_ref() else {
            return;
        };
        // The welcome is not a step to go back to, as Back doesn't offer it.
        if o.at > 0 && o.steps[o.at - 1] != Step::Welcome {
            self.onboarding_move(-1, window, cx);
        } else if !o.fresh {
            self.finish_onboarding(window, cx);
        }
    }

    /// A project was added. On the Project step that answers it, and the
    /// setup goes on; added any other way, the setup has done its job. After
    /// the setup, the first project opens the Phone tab it was asked for.
    pub(super) fn onboarding_project_added(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_ref() else {
            if std::mem::take(&mut self.phone_after_project) {
                self.open_phone_tab(window, cx);
            }
            return;
        };
        if o.step() == Step::Project && o.at + 1 < o.steps.len() {
            self.onboarding_move(1, window, cx);
        } else {
            self.finish_onboarding(window, cx);
        }
    }

    /// A change to the colors, applied at once and kept with the palettes.
    fn onboarding_change_look(
        &mut self,
        change: impl FnOnce(&mut crate::palettes::Saved),
        cx: &mut Context<Self>,
    ) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        let before = o.look.clone();
        change(&mut o.look);
        if o.look == before {
            return;
        }
        let saved = o.look.clone();
        crate::palettes::apply(&saved);
        let version = crate::palettes::next_version();
        cx.background_spawn(crate::guard::task(async move {
            crate::palettes::store_version(&saved, version);
        }))
        .detach();
        cx.refresh_windows();
        cx.notify();
    }

    /// The crash reports switch, answered as the dialog answers it: followed
    /// for this run either way; unsaved, the question comes back next launch.
    fn onboarding_toggle_reports(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.onboarding.as_mut() else {
            return;
        };
        o.reports = !o.reports;
        set_reports(o.reports);
        cx.notify();
    }

    /// Adds what the Project step's picker has highlighted.
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
            // As in the picker's field: Enter adds what it has highlighted.
            Step::Project => self.onboarding_add(window, cx),
            _ if o.at + 1 == o.steps.len() => self.finish_onboarding(window, cx),
            _ => self.onboarding_move(1, window, cx),
        }
    }

    /// Closes the setup for good: finished, or skipped. Someone who saw the
    /// crash reports switch and left it off has answered no; the question is
    /// still asked when the step was passed over.
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
        if o.reports_seen
            && crate::telemetry::should_ask()
            && crate::telemetry::env_override().is_none()
        {
            set_reports(o.reports);
        }
        cx.background_spawn(crate::guard::task(async { onboarding::set_done() }))
            .detach();
        for view in onboarding::views_set_up(o.fresh, o.github, o.pair_phone) {
            self.views
                .extensions
                .update(cx, |v, cx| v.set_view(view, true, cx));
        }
        if self.has_projects() {
            // Someone new lands on the start screen with a few keys to know.
            self.setup_tips = o.fresh;
            self.phone_after_project = false;
            window.focus(&self.composer_focus, cx);
            if o.pair_phone {
                self.open_phone_tab(window, cx);
            }
        } else {
            // Pairing needs a project; it waits for the first one.
            self.phone_after_project = o.pair_phone;
            window.focus(&self.focus, cx);
        }
        self.ask_telemetry(window, cx);
        self.open_pending_survey(window, cx);
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
        let page = if o.step() == Step::Welcome {
            self.onboarding_welcome(o, window, cx)
        } else {
            self.onboarding_page(o, window, cx)
        };
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
                        "escape" => this.onboarding_back(window, cx),
                        _ => {}
                    }
                }))
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(page)
                .into_any_element(),
        )
    }

    /// The bar along the top: the window moves by it, the traffic lights sit
    /// in it on macOS, and it holds the steps and the way out.
    fn onboarding_bar(
        &self,
        o: &Onboarding,
        steps: Option<AnyElement>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let leave = ui::button(
            "onboarding-skip",
            if o.fresh { "Skip setup" } else { "Close" },
            ButtonKind::Ghost,
            true,
        )
        .on_click(cx.listener(|this, _, window, cx| this.finish_onboarding(window, cx)));
        let stop = |d: gpui::Div| {
            // A press on a step or a button isn't a drag of the window.
            d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(ui::BAR_H))
            .pl(ui::icon_inset().max(crate::ui::traffic_lights_inset(window)))
            .gap(ui::sp(16.))
            .on_mouse_down(MouseButton::Left, |ev, window, _| {
                crate::ui::drag_window(ev, window)
            })
            .when(cfg!(windows), |d| {
                d.window_control_area(WindowControlArea::Drag)
            })
            .when_some(steps, |d, steps| {
                d.when(window.viewport_size().width >= px(BAR_TITLE_MIN_W), |d| {
                    d.child(
                        ui::text(ui::Type::Label, theme::text(), "Set up Arakun")
                            .font_weight(FontWeight::SEMIBOLD)
                            .flex_none(),
                    )
                })
                .child(stop(div().flex().min_w_0().overflow_hidden().child(steps)))
            })
            .child(div().flex_1())
            .child(stop(div().flex().child(leave)))
            .when(cfg!(not(target_os = "macos")), |d| {
                d.child(
                    caption_buttons()
                        .child(
                            window_button("win-min", "minus-sign", 14., false)
                                .on_click(|_, window, _| window.minimize_window()),
                        )
                        .child(max_button(window.is_maximized()))
                        .child(
                            window_button("win-close", "cancel01", 14., true)
                                .on_click(|_, window, cx| crate::background::close(window, cx)),
                        ),
                )
            })
    }

    /// The steps in a row, each with the answer it recorded once done. A
    /// step already seen can be gone back to; ahead goes through Next.
    fn onboarding_steps(
        &self,
        o: &Onboarding,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let answers = window.viewport_size().width >= px(BAR_ANSWERS_MIN_W);
        let mut row = div().flex().items_center().gap(ui::sp(4.));
        for (n, (i, step)) in o.questions().enumerate() {
            let (now, done) = (i == o.at, i < o.at);
            if n > 0 {
                row = row.child(ui::icon("arrow-right01", 12.).text_color(theme::text_faint()));
            }
            let mark = div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(20.))
                .rounded_full()
                .ty(ui::Type::Caption)
                .font_family(theme::FONT_MONO);
            let mark = if done {
                mark.border_1()
                    .border_color(theme::border_subtle())
                    .child(ui::icon("tick02", 12.).text_color(theme::success()))
            } else if now {
                mark.bg(theme::content())
                    .text_color(theme::background())
                    .child((n + 1).to_string())
            } else {
                mark.border_1()
                    .border_color(theme::border())
                    .text_color(theme::text_faint())
                    .child((n + 1).to_string())
            };
            let value = (done && answers)
                .then(|| self.onboarding_answer(o, step))
                .flatten();
            row = row.child(
                div()
                    .id(("onboarding-to", i))
                    .flex()
                    .flex_none()
                    .whitespace_nowrap()
                    .items_center()
                    .gap(ui::sp(8.))
                    .h(px(ui::BUTTON_H))
                    .pl(ui::sp(4.))
                    .pr(ui::sp(12.))
                    .rounded_full()
                    .ty(ui::Type::Label)
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
                    .when(done, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(theme::fill_hover()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.onboarding_go(i, window, cx)
                            }))
                    })
                    .child(mark)
                    .child(step.label())
                    .when_some(value, |d, v| {
                        d.child(
                            ui::text(ui::Type::Caption, theme::text_faint(), v)
                                .max_w(px(BAR_ANSWER_MAX_W))
                                .truncate(),
                        )
                    }),
            );
        }
        row.into_any_element()
    }

    /// What a step recorded, for the bar and the preview's label.
    fn onboarding_answer(&self, o: &Onboarding, step: Step) -> Option<SharedString> {
        match step {
            Step::Welcome | Step::Connect => None,
            Step::Agents => Some(crate::model::default_agent().label().into()),
            Step::Project => Some(if self.has_projects() {
                self.project().name.clone()
            } else {
                "Later".into()
            }),
            Step::Look => {
                let palette = o.look.active().name;
                let palette = palette.trim_start_matches("Arakun ").to_string();
                Some(match self.sidebar_preset() {
                    Some(p) => format!("{palette}, {}", p.label()).into(),
                    None => palette.into(),
                })
            }
        }
    }

    /// The first thing someone new sees: the raccoon, one line, and what
    /// Arakun found on the machine as it looks.
    fn onboarding_welcome(
        &self,
        o: &Onboarding,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut found = ui::surface_card()
            .gap(ui::sp(2.))
            .p(ui::sp(12.))
            .child(ui::overline("Checking this machine").pb(ui::sp(8.)));
        match &o.agents {
            None => found = found.child(scan_row(None, "Agents", "Looking")),
            Some(rows) => {
                for row in rows {
                    let says = match row.state {
                        Readiness::Ready => "Ready",
                        Readiness::OnFirstUse => "Set up at its first session",
                        Readiness::Missing => "Not installed",
                    };
                    found = found.child(scan_row(
                        Some(row.state != Readiness::Missing),
                        row.agent.label(),
                        says,
                    ));
                }
            }
        }
        let (gh_found, gh_says) = match o.github {
            None => (None, "Asking the GitHub CLI"),
            Some(GitHub::Connected) => (Some(true), "Signed in"),
            Some(GitHub::SignedOut) => (Some(false), "Signed out"),
            Some(GitHub::NoGh) => (Some(false), "The GitHub CLI is not installed"),
        };
        found = found.child(scan_row(gh_found, "GitHub", gh_says));
        let start = ui::button("onboarding-start", "Start", ButtonKind::Primary, true)
            .on_click(cx.listener(|this, _, window, cx| this.onboarding_move(1, window, cx)));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.onboarding_bar(o, None, window, cx))
            .child(
                div()
                    .id("onboarding-welcome")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .items_center()
                    .px(ui::sp(24.))
                    .pb(ui::sp(24.))
                    .child(div().flex_shrink(1.).flex_basis(px(64.)).min_h(px(16.)))
                    .child(div().flex_none().w(px(FACE_W)).child(o.face.clone()))
                    .child(
                        ui::text(ui::Type::Display, theme::text(), Step::Welcome.title())
                            .mt(ui::sp(16.))
                            .text_center(),
                    )
                    .child(
                        ui::text(
                            ui::Type::Body,
                            theme::text_subtle(),
                            "One window for the coding agents you already have. Four short questions, and most answers are already filled in.",
                        )
                        .mt(ui::sp(8.))
                        .max_w(px(WELCOME_W))
                        .text_center(),
                    )
                    .child(div().flex_none().w(px(WELCOME_W)).mt(ui::sp(32.)).child(found))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(16.))
                            .mt(ui::sp(24.))
                            .child(keys_hint(&["Enter"], "start, even before the check is done"))
                            .child(start),
                    ),
            )
            .into_any_element()
    }

    /// A step: the bar, then the questions beside the live preview.
    fn onboarding_page(
        &self,
        o: &Onboarding,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let step = o.step();
        let title = match step {
            Step::Project if !o.fresh => "Add another project",
            step => step.title(),
        };
        let body = match step {
            Step::Welcome => div().into_any_element(),
            Step::Agents => self.onboarding_agents(o, cx),
            Step::Project => match &o.picker {
                Some((picker, _)) => picker.clone().into_any_element(),
                None => div().into_any_element(),
            },
            Step::Look => self.onboarding_look(o, cx),
            Step::Connect => self.onboarding_connect(o, cx),
        };
        let wide = window.viewport_size().width >= px(PREVIEW_MIN_W);
        let form = div()
            .flex()
            .flex_col()
            .min_h_0()
            .when(wide, |d| d.flex_none().w(px(FORM_W)))
            .when(!wide, |d| d.flex_1().min_w_0())
            .child(
                div()
                    .id("onboarding-step")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(ui::sp(24.))
                    .px(ui::sp(40.))
                    .pt(ui::sp(48.))
                    .pb(ui::sp(24.))
                    .child(ui::page_title(title, Some(step.blurb().into_any_element())))
                    .child(body),
            )
            .child(self.onboarding_footer(o, cx));
        let sheet = ui::card()
            .flex()
            .flex_1()
            .min_h_0()
            .bg(theme::sheet())
            .child(form)
            .when(wide, |d| d.child(self.onboarding_preview(o, window, cx)));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.onboarding_bar(o, Some(self.onboarding_steps(o, window, cx)), window, cx))
            .child(sheet)
            .into_any_element()
    }

    /// The window as it will look, drawn small: the real sidebar, and the
    /// start screen beside it. It only shows; clicks don't reach it.
    fn onboarding_preview(
        &self,
        o: &Onboarding,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let agent = crate::model::default_agent();
        let project = self.has_projects().then(|| self.project().name.clone());
        let note: SharedString = match o.step() {
            Step::Welcome | Step::Agents => format!("{} starts new sessions", agent.label()).into(),
            Step::Project => match &project {
                Some(name) => format!("{name}, ready for a session").into(),
                None => "no project yet".into(),
            },
            Step::Look => self.onboarding_answer(o, Step::Look).unwrap_or_default(),
            Step::Connect => match o.github {
                Some(GitHub::Connected) => "GitHub joins the sidebar when you finish".into(),
                _ => "nothing connected yet".into(),
            },
        };
        let heading = match &project {
            Some(name) => format!("What should we work on in {name}?"),
            None => "What should we work on?".to_string(),
        };
        let composer = div()
            .flex()
            .flex_col()
            .gap(ui::sp(10.))
            .p(ui::sp(12.))
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(ui::text(
                ui::Type::Body,
                theme::text_faint(),
                super::composer::PLACEHOLDER,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(6.))
                    .ty(ui::Type::Label)
                    .text_color(theme::text_muted())
                    .child(agent.icon(14.))
                    .child(agent.label()),
            );
        let tab = div()
            .flex()
            .items_center()
            .gap(ui::sp(6.))
            .h(px(ui::BUTTON_H))
            .px(ui::sp(10.))
            .rounded(ui::rad(ui::RADIUS))
            .bg(theme::fill_selected())
            .ty(ui::Type::Label)
            .child(agent.icon(14.))
            .child("New session");
        let main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(ui::BAR_H))
                    .child(tab),
            )
            .child(
                ui::card()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .px(ui::sp(24.))
                    .pb(ui::sp(48.))
                    .bg(theme::sheet())
                    .child(
                        ui::text(ui::Type::Title, theme::text(), heading)
                            .px(ui::sp(10.))
                            .mb(ui::sp(12.)),
                    )
                    .child(composer),
            );
        let rail_right = self.sidebar.style.side == crate::sidebar::Side::Right;
        let app = div()
            .flex()
            .flex_1()
            .min_h_0()
            .gap(crate::ui::window_gap_px())
            .p(crate::ui::window_gap_px())
            .rounded(ui::rad(ui::RADIUS_XL))
            .border_1()
            .border_color(theme::border())
            .bg(theme::window_bg())
            .overflow_hidden()
            .when(!rail_right, |d| d.child(self.render_rail(window, cx)))
            .child(main)
            .when(rail_right, |d| d.child(self.render_rail(window, cx)));
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(ui::sp(16.))
            .m(ui::sp(8.))
            .p(ui::sp(24.))
            .rounded(ui::nested(ui::RADIUS_XL, 8.))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::background())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(div().size(px(6.)).rounded_full().bg(theme::success()))
                    .child(ui::text(
                        ui::Type::Overline,
                        theme::text_subtle(),
                        format!("YOUR ARAKUN · {}", note.to_uppercase()),
                    )),
            )
            .child(app)
            .child(
                div()
                    .id("onboarding-preview-cover")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude(),
            )
            .into_any_element()
    }

    /// The keys, then Back and the step's main button, which names where it
    /// goes.
    fn onboarding_footer(&self, o: &Onboarding, cx: &mut Context<Self>) -> gpui::Div {
        let step = o.step();
        let mut actions = div().flex().items_center().gap(ui::sp(8.));
        if o.at > 0 && o.steps[o.at - 1] != Step::Welcome {
            actions = actions.child(
                ui::button("onboarding-back", "Back", ButtonKind::Ghost, true).on_click(
                    cx.listener(|this, _, window, cx| this.onboarding_move(-1, window, cx)),
                ),
            );
        }
        let next_label = o
            .steps
            .get(o.at + 1)
            .map(|next| format!("Next: {}", next.label()));
        let next = |label: String, kind: ButtonKind| {
            ui::button("onboarding-next", label, kind, true)
                .on_click(cx.listener(|this, _, window, cx| this.onboarding_move(1, window, cx)))
        };
        actions = match (step, next_label) {
            (Step::Project, Some(next_label)) => {
                let target = o
                    .picker
                    .as_ref()
                    .and_then(|(p, _)| p.read(cx).target_name());
                let label = match &target {
                    Some(name) => format!("Add {name}"),
                    None => "Add project".to_string(),
                };
                let can = target.is_some();
                let add = ui::button_with(
                    "onboarding-add",
                    Some("folder-add"),
                    label,
                    ButtonKind::Primary,
                    can,
                )
                .max_w(px(240.))
                .when(can, |d| {
                    d.on_click(cx.listener(|this, _, window, cx| this.onboarding_add(window, cx)))
                });
                let skip = if self.has_projects() {
                    next_label
                } else {
                    "Not now".to_string()
                };
                actions.child(next(skip, ButtonKind::Ghost)).child(add)
            }
            (_, Some(next_label)) => actions.child(next(next_label, ButtonKind::Primary)),
            (_, None) => {
                let label = match (o.fresh, self.has_projects()) {
                    (true, true) => format!("Open {}", self.project().name),
                    _ => "Done".to_string(),
                };
                actions.child(
                    ui::button("onboarding-done", label, ButtonKind::Primary, true)
                        .max_w(px(240.))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.finish_onboarding(window, cx)),
                        ),
                )
            }
        };
        let hints = match step {
            Step::Project => div(),
            _ => div()
                .flex()
                .items_center()
                .gap(ui::sp(12.))
                .child(keys_hint(
                    &["Enter"],
                    match (o.at + 1 == o.steps.len(), o.fresh && self.has_projects()) {
                        (true, true) => "open",
                        (true, false) => "done",
                        (false, _) => "next",
                    },
                ))
                .child(keys_hint(&["Esc"], "back")),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(12.))
            .h(px(64.))
            .px(ui::sp(40.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(hints.flex_1())
            .child(actions)
    }

    fn onboarding_agents(&self, o: &Onboarding, cx: &mut Context<Self>) -> AnyElement {
        let Some(rows) = &o.agents else {
            return ui::loading("Looking for agents").into_any_element();
        };
        let picked = crate::model::default_agent();
        let usable = rows
            .iter()
            .filter(|r| r.state != Readiness::Missing)
            .count();
        let summary = div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .child(ui::icon("checkmark-circle02", 12.).text_color(theme::success()))
            .child(
                ui::text(
                    ui::Type::Mono,
                    theme::text_faint(),
                    format!(
                        "Checked this machine · {usable} of {} agents found",
                        rows.len()
                    ),
                )
                .flex_1(),
            )
            .child(
                ui::button(
                    "onboarding-agents-again",
                    "Check again",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.onboarding_check_agents(cx))),
            );
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
            let line = pick_row(
                ("onboarding-agent", i),
                agent.icon(16.),
                agent.label(),
                says,
                on,
            );
            list = list.child(if usable {
                let tag = (row.state == Readiness::OnFirstUse)
                    .then(|| ui::badge("Set up at first session", ui::Tone::Neutral));
                line.cursor_pointer()
                    .when(!on, |d| d.hover(|s| s.bg(theme::fill_hover())))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.onboarding_pick_agent(agent, cx)),
                    )
                    .when_some(tag, |d, tag| d.child(tag))
                    .child(ui::radio(on))
            } else {
                // Nothing to pick, only where to get it.
                let (label, url) = onboarding::install_link(agent);
                line.text_color(theme::text_subtle()).child(
                    link(("onboarding-agent-get", i), label).on_click(|_, _, cx| cx.open_url(url)),
                )
            });
        }
        let also = (o.github == Some(GitHub::Connected)).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(8.))
                .ty(ui::Type::Body)
                .text_color(theme::text_muted())
                .child(ui::overline("Also found").pl(ui::sp(4.)))
                .child(crate::telemetry_dialog::point(
                    "github",
                    "GitHub, signed in through the GitHub CLI. Connect has it already.",
                ))
        });
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .child(summary)
            .child(list)
            .when_some(also, |d, also| d.child(also))
            .child(note(
                "An agent that is not signed in says how when you start a session with it.",
            ))
            .into_any_element()
    }

    fn onboarding_look(&self, o: &Onboarding, cx: &mut Context<Self>) -> AnyElement {
        let light = theme::is_light();
        let mode =
            |id: &'static str, label: &'static str, on: bool| {
                ui::segment(id, None, label, on == light).on_click(cx.listener(
                    move |this, _, _, cx| this.onboarding_change_look(|s| s.light = on, cx),
                ))
            };
        let scheme = ui::segmented()
            .child(mode("onboarding-dark", "Dark", false))
            .child(mode("onboarding-light", "Light", true))
            .into_any_element();
        let active = o.look.active().id;
        let palettes: Vec<AnyElement> = theme::Palette::built_in()
            .into_iter()
            .enumerate()
            .map(|(i, p)| palette_tile(i, &p, p.id == active, cx))
            .collect();
        let current = self.sidebar_preset();
        let presets: Vec<AnyElement> = Preset::ALL
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let on = current == Some(p);
                div()
                    .id(("onboarding-preset", i))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .py(ui::sp(8.))
                    .px(ui::sp(10.))
                    .rounded(ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(if on {
                        theme::active().opacity(0.55)
                    } else {
                        theme::stroke()
                    })
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(4.))
                            .child(
                                ui::text(ui::Type::Label, theme::text(), p.label())
                                    .font_weight(FontWeight::MEDIUM)
                                    .flex_1(),
                            )
                            .when(on, |d| {
                                d.child(ui::icon("tick02", 12.).text_color(theme::active()))
                            }),
                    )
                    .child(ui::text(
                        ui::Type::Caption,
                        theme::text_subtle(),
                        preset_blurb(p),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.apply_sidebar_preset(p, cx)))
                    .into_any_element()
            })
            .collect();
        let group = |name: &'static str, end: AnyElement, rows: gpui::Div| {
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .min_h(px(ui::BUTTON_H))
                        .child(ui::overline(name).pl(ui::sp(4.)))
                        .child(end),
                )
                .child(rows)
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(24.))
            .child(group(
                "Color scheme",
                scheme,
                grid(palettes, PALETTES_PER_ROW),
            ))
            .child(group(
                "Sidebar",
                ui::text(
                    ui::Type::Caption,
                    theme::text_faint(),
                    "Arrange it piece by piece in Settings",
                )
                .into_any_element(),
                grid(presets, PRESETS_PER_ROW),
            ))
            .into_any_element()
    }

    fn onboarding_connect(&self, o: &Onboarding, cx: &mut Context<Self>) -> AnyElement {
        use crate::telemetry_dialog::{DETAILS_URL, POINTS, point};
        let github = match o.github {
            None => ui::spinner(14., theme::text_faint()).into_any_element(),
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
            _ => "Reviews and checks in the sidebar, through the GitHub CLI's sign-in",
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
        // An environment variable decides for the user: show what it decided,
        // which can be on as well as off, and leave the switch where it is.
        let forced = crate::telemetry::env_override();
        let forced_on = crate::telemetry::effective() != crate::telemetry::Consent::NONE;
        // Crashes without usage counts, or the other way round, was chosen in
        // Settings: one switch can't say that, and a click would overwrite it.
        let split = crate::telemetry::consent().is_some_and(|c| {
            c != crate::telemetry::Consent::ALL && c != crate::telemetry::Consent::NONE
        });
        let reports_says: SharedString = match (forced, o.reports) {
            // The variable wins over the saved answer, split or not.
            (Some(var), _) => format!(
                "{} while {var} is set.",
                if forced_on { "On" } else { "Off" }
            )
            .into(),
            (None, _) if split => "Partly on, as chosen in Settings › Privacy.".into(),
            (None, true) => "On. They tell us what breaks and what gets used.".into(),
            (None, false) => {
                "Off until you turn them on. They tell us what breaks and what gets used.".into()
            }
        };
        let fixed = forced.is_some() || split;
        let shown_on = if forced.is_some() {
            forced_on
        } else if split {
            true
        } else {
            o.reports
        };
        let reports = div()
            .id("onboarding-reports")
            .flex()
            .child(ui::switch(shown_on))
            .when(fixed, |d| d.opacity(0.5))
            .when(!fixed, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.onboarding_toggle_reports(cx)))
            })
            .into_any_element();
        let row = |id: &'static str,
                   icon_name: &'static str,
                   name,
                   says: SharedString,
                   end: AnyElement| {
            let lead = ui::icon(icon_name, 16.).text_color(theme::text_muted());
            pick_row(id, lead.into_any_element(), name, says, false).child(end)
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(24.))
            .child(
                ui::surface_card()
                    .p(ui::sp(4.))
                    .gap(ui::sp(2.))
                    .child(row(
                        "onboarding-gh",
                        "github",
                        "GitHub",
                        github_says.into(),
                        github,
                    ))
                    .child(row(
                        "onboarding-phone-row",
                        "signal-full02",
                        "Phone remote",
                        if o.fresh && !self.has_projects() {
                            "Follow sessions and answer agents from your phone. The Phone tab opens once you have a project."
                        } else {
                            "Follow sessions and answer agents from your phone"
                        }
                        .into(),
                        phone,
                    ))
                    .child(row(
                        "onboarding-reports-row",
                        "shield-alert",
                        "Crash reports and usage counts",
                        reports_says,
                        reports,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(10.))
                    .ty(ui::Type::Body)
                    .text_color(theme::text_muted())
                    .child(ui::overline("What a crash report holds").pl(ui::sp(4.)))
                    .children(POINTS.map(|(icon_name, text)| point(icon_name, text)))
                    .child(
                        // In line with the sentences, past their icons.
                        div().flex().pl(ui::sp(24.)).child(
                            link("onboarding-reports-details", "What is sent")
                                .on_click(|_, _, cx| cx.open_url(DETAILS_URL)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

/// The answer to the question about reports, as the dialog records it.
fn set_reports(yes: bool) {
    use crate::telemetry::Consent;
    let choice = if yes { Consent::ALL } else { Consent::NONE };
    if let Err(e) = crate::telemetry::set_consent(choice) {
        eprintln!("arakun: {e}");
    }
}

/// A layout in a few words, for its tile.
fn preset_blurb(p: Preset) -> &'static str {
    match p {
        Preset::Switcher => "One project at a time",
        Preset::Focused => "Inbox and projects",
        Preset::Developer => "Git in its own group",
        Preset::Notion => "Your pins up top",
        Preset::Compact => "Icons, names on hover",
        Preset::Everything => "Every view, listed",
    }
}

/// Tiles in rows of `per_row`, the last row kept to the same widths.
fn grid(tiles: Vec<AnyElement>, per_row: usize) -> gpui::Div {
    let mut rows = div().flex().flex_col().gap(ui::sp(8.));
    let mut tiles = tiles.into_iter().peekable();
    while tiles.peek().is_some() {
        let mut row = div().flex().gap(ui::sp(8.));
        for _ in 0..per_row {
            row = match tiles.next() {
                Some(tile) => row.child(tile),
                None => row.child(div().flex_1().min_w_0()),
            };
        }
        rows = rows.child(row);
    }
    rows
}

/// One palette as a small window drawn in it, as Settings shows them.
fn palette_tile(i: usize, p: &theme::Palette, on: bool, cx: &mut Context<Shell>) -> AnyElement {
    let id = p.id.clone();
    let bar = |w: f32, c: gpui::Hsla| div().h(px(3.)).w(gpui::relative(w)).rounded_full().bg(c);
    // 5 px (padding and border) inside the tile's 12 px corner.
    let preview = div()
        .flex()
        .h(px(40.))
        .rounded(ui::nested(ui::RADIUS_LG, 5.))
        .overflow_hidden()
        .bg(p.background())
        .child(
            div()
                .w(gpui::relative(0.34))
                .h_full()
                .flex()
                .flex_col()
                .gap(ui::sp(4.))
                .p(ui::sp(5.))
                .bg(p.surface(0.07))
                .child(bar(0.7, p.accent()))
                .child(bar(0.9, p.content().opacity(0.22))),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(ui::sp(4.))
                .p(ui::sp(6.))
                .child(bar(0.8, p.content().opacity(0.8)))
                .child(bar(0.55, p.content().opacity(0.25)))
                .child(div().size(px(6.)).rounded_full().bg(p.accent())),
        );
    let name = p.name.trim_start_matches("Arakun ").to_string();
    div()
        .id(("onboarding-palette", i))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(ui::sp(5.))
        .p(ui::sp(4.))
        .rounded(ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(if on {
            theme::active().opacity(0.55)
        } else {
            theme::stroke()
        })
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()))
        .child(preview)
        .child(
            div()
                .flex()
                .items_center()
                .gap(ui::sp(4.))
                .px(ui::sp(2.))
                .child(
                    ui::text(
                        ui::Type::Caption,
                        if on {
                            theme::text()
                        } else {
                            theme::text_muted()
                        },
                        name,
                    )
                    .font_weight(FontWeight::MEDIUM)
                    .flex_1()
                    .min_w_0()
                    .truncate(),
                )
                .when(on, |d| {
                    d.child(ui::icon("tick02", 12.).text_color(theme::active()))
                }),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let id = id.clone();
            this.onboarding_change_look(|s| s.current = id, cx)
        }))
        .into_any_element()
}

/// One line of the welcome's check: a tick, a spinner or an empty ring,
/// what was looked for, and what was found.
fn scan_row(found: Option<bool>, name: &'static str, says: &'static str) -> gpui::Div {
    let mark = match found {
        None => ui::spinner(14., theme::text_faint()).into_any_element(),
        Some(true) => ui::icon("checkmark-circle02", 14.)
            .text_color(theme::success())
            .into_any_element(),
        Some(false) => ui::icon("circle-dashed", 14.)
            .text_color(theme::text_faint())
            .into_any_element(),
    };
    div()
        .flex()
        .items_center()
        .gap(ui::sp(10.))
        .min_h(px(28.))
        .px(ui::sp(4.))
        .child(div().flex().flex_none().child(mark))
        .child(
            ui::text(
                ui::Type::Body,
                if found == Some(false) {
                    theme::text_subtle()
                } else {
                    theme::text()
                },
                name,
            )
            .flex_1(),
        )
        .child(ui::text(ui::Type::Mono, theme::text_faint(), says))
}

/// Keys and what they do, for a footer.
fn keys_hint(keys: &[&str], what: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(ui::sp(6.))
        .child(ui::shortcut(keys))
        .child(ui::text(ui::Type::Label, theme::text_faint(), what))
}

/// A name with a line under it, at the start of a row.
fn titled(name: &'static str, says: impl Into<SharedString>) -> gpui::Div {
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
    says: impl Into<SharedString>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(ui::sp(12.))
        .min_h(px(48.))
        .py(ui::sp(6.))
        .px(ui::sp(10.))
        .rounded(ui::nested(ui::RADIUS_XL, 5.))
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
