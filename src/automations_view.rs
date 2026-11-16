//! The Automations dialog: a project's saved shell commands and agent prompts,
//! run by hand or on a schedule.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::input::{InputEvent, TextInput};
use crate::model::{Agent, Automation, unix_now};
use crate::schedule::{self, Schedule};
use crate::theme;
use crate::ui::{self, ButtonKind, icon, icon_button};

pub enum AutomationsEvent {
    Save {
        index: Option<usize>,
        automation: Automation,
    },
    Delete(usize),
    Run(usize),
    SetEnabled(usize, bool),
    /// Show the session the last run of this agent automation started.
    OpenSession(usize),
    /// Stop a rule made in the palette from waiting.
    CancelRule(u64),
    Dismissed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Every {
    Manual,
    Hourly,
    Daily,
    Weekdays,
    Weekly,
}

struct Template {
    name: &'static str,
    agent: Option<Agent>,
    body: &'static str,
    schedule: Schedule,
}

const TEMPLATES: &[Template] = &[
    Template {
        name: "Find critical bugs",
        agent: Some(Agent::Droid),
        body: "Review the git history of the last day in this repo for high-severity correctness bugs \
               (crashes, data loss, security holes, broken error handling). Report each finding with the \
               file, the commit and a suggested fix. Do not change any code.",
        schedule: Schedule::Weekdays { hour: 9, minute: 0 },
    },
    Template {
        name: "Weekly changelog",
        agent: Some(Agent::Droid),
        body: "Write a concise changelog of this repo covering the commits of the last 7 days, grouped into \
               features, fixes and internal changes. Show it here; do not commit anything.",
        schedule: Schedule::Weekly {
            day: 5,
            hour: 16,
            minute: 0,
        },
    },
    Template {
        name: "Audit dependencies",
        agent: Some(Agent::Claude),
        body: "Audit this repo's dependencies: list outdated or vulnerable packages, what upgrading each one \
               would involve, and which upgrades are safe to do now. Do not change any files.",
        schedule: Schedule::Weekly {
            day: 1,
            hour: 10,
            minute: 0,
        },
    },
    Template {
        name: "Run the tests",
        agent: None,
        body: "cargo test",
        schedule: Schedule::Manual,
    },
];

pub struct AutomationsDialog {
    project: SharedString,
    items: Vec<Automation>,
    /// Rules made from a sentence in the palette: they wait for something to happen.
    rules: Vec<crate::palette::rules::Rule>,
    /// Environments it can run in, the main checkout first.
    envs: Vec<(PathBuf, SharedString)>,
    /// `None` shows the list; `Some(None)` a new automation, `Some(Some(i))` editing one.
    editing: Option<Option<usize>>,
    name: Entity<TextInput>,
    body: Entity<TextInput>,
    time: Entity<TextInput>,
    agent: Option<Agent>,
    every: Every,
    day: u8,
    env: Option<PathBuf>,
    error: Option<SharedString>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<AutomationsEvent> for AutomationsDialog {}

impl AutomationsDialog {
    pub fn new(
        project: SharedString,
        items: Vec<Automation>,
        envs: Vec<(PathBuf, SharedString)>,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| TextInput::new("Morning bug hunt", cx));
        let body = cx.new(|cx| TextInput::new("What the agent should do", cx));
        let time = cx.new(|cx| TextInput::new("09:00", cx).mono());
        let mut subs = Vec::new();
        for input in [&name, &body, &time] {
            subs.push(cx.subscribe_in(
                input,
                window,
                |this: &mut Self, _, ev, window, cx| match ev {
                    InputEvent::Submit => this.submit(window, cx),
                    InputEvent::Cancel => this.back(window, cx),
                    InputEvent::Changed => {
                        this.error = None;
                        cx.notify();
                    }
                    _ => {}
                },
            ));
        }
        let focus = cx.focus_handle();
        let mut this = Self {
            project,
            items,
            rules: Vec::new(),
            envs,
            editing: None,
            name,
            body,
            time,
            agent: Some(Agent::Droid),
            every: Every::Manual,
            day: 1,
            env: None,
            error: None,
            focus,
            _subs: subs,
        };
        if create {
            this.edit(None, window, cx);
        } else {
            window.focus(&this.focus, cx);
        }
        this
    }

    pub fn set_items(&mut self, items: Vec<Automation>, cx: &mut Context<Self>) {
        self.items = items;
        cx.notify();
    }

    /// The palette's rules: waiting ones first, then the last finished.
    pub fn set_rules(&mut self, rules: Vec<crate::palette::rules::Rule>, cx: &mut Context<Self>) {
        self.rules = shown_rules(rules);
        cx.notify();
    }

    fn render_rule(
        &self,
        r: &crate::palette::rules::Rule,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::palette::rules::State;
        let id = r.id;
        let waiting = r.state == State::Waiting;
        let steps: Vec<String> = r.steps.iter().map(|s| s.describe()).collect();
        let mut facts = vec![format!(
            "When {} · {}",
            r.trigger.describe(),
            steps.join(", then ")
        )];
        match &r.state {
            State::Waiting => {
                if !r.note.is_empty() {
                    facts.push(r.note.clone());
                }
                facts.push(format!("expires {}", schedule::when(r.expires, now)));
            }
            State::Running => facts.push("running".into()),
            State::Done => facts.push("done".into()),
            State::Expired => facts.push("expired".into()),
            State::Cancelled => facts.push("cancelled".into()),
            State::Failed(why) => facts.push(format!("failed: {why}")),
        }
        div()
            .id(("auto-rule", id as usize))
            .flex()
            .items_center()
            .gap(crate::ui::sp(10.))
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .hover(|s| s.bg(theme::hover()))
            .when(!waiting, |d| d.opacity(0.55))
            .child(
                div()
                    .flex_none()
                    .w(px(18.))
                    .flex()
                    .justify_center()
                    .child(icon(r.trigger.icon(), 16.).text_color(theme::text_subtle())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_MD))
                            .truncate()
                            .child(r.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .truncate()
                            .child(facts.join(" · ")),
                    ),
            )
            .when(waiting, |d| {
                d.child(
                    ui::button(
                        ("auto-cancel", id as usize),
                        "Cancel",
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(AutomationsEvent::CancelRule(id))),
                    ),
                )
            })
            .into_any_element()
    }

    fn edit(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let a = index.and_then(|i| self.items.get(i)).cloned();
        self.editing = Some(index);
        self.error = None;
        match a {
            Some(a) => {
                self.load(&a.name, a.agent(), &a.body, a.schedule, cx);
                self.env = a.env.clone();
            }
            None => {
                self.load("", Some(Agent::Droid), "", Schedule::Manual, cx);
                self.env = None;
            }
        }
        window.focus(&self.name.focus_handle(cx), cx);
        cx.notify();
    }

    fn load(
        &mut self,
        name: &str,
        agent: Option<Agent>,
        body: &str,
        schedule: Schedule,
        cx: &mut Context<Self>,
    ) {
        self.name
            .update(cx, |i, cx| i.set_text(name.to_string(), cx));
        self.body
            .update(cx, |i, cx| i.set_text(body.to_string(), cx));
        self.agent = agent;
        let (every, time) = match schedule {
            Schedule::Manual => (Every::Manual, "09:00".to_string()),
            Schedule::Hourly { minute } => (Every::Hourly, format!("{minute:02}")),
            Schedule::Daily { hour, minute } => (Every::Daily, format!("{hour:02}:{minute:02}")),
            Schedule::Weekdays { hour, minute } => {
                (Every::Weekdays, format!("{hour:02}:{minute:02}"))
            }
            Schedule::Weekly { day, hour, minute } => {
                self.day = day;
                (Every::Weekly, format!("{hour:02}:{minute:02}"))
            }
        };
        self.every = every;
        self.time.update(cx, |i, cx| i.set_text(time, cx));
    }

    fn set_every(&mut self, every: Every, cx: &mut Context<Self>) {
        // Hourly takes a minute, the rest a time of day: keep the field sensible.
        let text = self.time.read(cx).text().to_string();
        let fixed = match (self.every == Every::Hourly, every == Every::Hourly) {
            (false, true) => {
                Some(schedule::parse_time(&text).map_or("00".into(), |(_, m)| format!("{m:02}")))
            }
            (true, false) => Some("09:00".to_string()),
            _ => None,
        };
        if let Some(t) = fixed {
            self.time.update(cx, |i, cx| i.set_text(t, cx));
        }
        self.every = every;
        self.error = None;
        cx.notify();
    }

    /// The schedule as typed, or why it is not valid.
    fn schedule(&self, cx: &App) -> Result<Schedule, &'static str> {
        let text = self
            .time
            .read(cx)
            .text()
            .trim()
            .trim_start_matches(':')
            .to_string();
        let time = || schedule::parse_time(&text).ok_or("Type the time as HH:MM, like 09:30");
        Ok(match self.every {
            Every::Manual => Schedule::Manual,
            Every::Hourly => match text.parse::<u8>() {
                Ok(minute) if minute < 60 => Schedule::Hourly { minute },
                _ => return Err("Type the minute of the hour, 0 to 59"),
            },
            Every::Daily => {
                let (hour, minute) = time()?;
                Schedule::Daily { hour, minute }
            }
            Every::Weekdays => {
                let (hour, minute) = time()?;
                Schedule::Weekdays { hour, minute }
            }
            Every::Weekly => {
                let (hour, minute) = time()?;
                Schedule::Weekly {
                    day: self.day,
                    hour,
                    minute,
                }
            }
        })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.editing else {
            return;
        };
        let name = self.name.read(cx).text().trim().to_string();
        let body = self.body.read(cx).text().trim().to_string();
        if name.is_empty() || body.is_empty() {
            self.error = Some("Both a name and the content are needed".into());
            cx.notify();
            return;
        }
        let schedule = match self.schedule(cx) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e.into());
                cx.notify();
                return;
            }
        };
        let old = index.and_then(|i| self.items.get(i));
        let automation = Automation {
            name,
            body,
            agent: self.agent.map(|a| a.key().to_string()),
            schedule,
            enabled: old.is_none_or(|a| a.enabled),
            env: self.env.clone(),
            // Only times from now on count, so a new schedule never fires for the past.
            checked: unix_now(),
            last_run: old.and_then(|a| a.last_run),
            last_session: old.and_then(|a| a.last_session.clone()),
        };
        cx.emit(AutomationsEvent::Save { index, automation });
        self.editing = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        } else {
            cx.emit(AutomationsEvent::Dismissed);
        }
    }

    fn env_name(&self, path: &Option<PathBuf>) -> Option<SharedString> {
        let path = path.as_ref()?;
        self.envs
            .iter()
            .skip(1)
            .find(|(p, _)| p == path)
            .map(|(_, n)| n.clone())
    }

    fn render_row(&self, i: usize, a: &Automation, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let mut facts = vec![a.schedule.describe()];
        if let Some(env) = self.env_name(&a.env) {
            facts.push(format!("in {env}"));
        }
        match a.next_run(now) {
            Some(next) => facts.push(format!("next {}", schedule::when(next, now))),
            None if a.schedule != Schedule::Manual => facts.push("paused".into()),
            None => {}
        }
        if let Some(last) = a.last_run {
            facts.push(format!("last {}", schedule::when(last, now)));
        }
        let lead: AnyElement = match a.agent() {
            Some(agent) => crate::shell::agent_icon(agent, 16.),
            None => icon("flash", 16.)
                .text_color(theme::text_subtle())
                .into_any_element(),
        };
        let scheduled = a.schedule != Schedule::Manual;
        let has_session = a.agent.is_some() && a.last_session.is_some();
        div()
            .id(("auto-row", i))
            .flex()
            .items_center()
            .gap(crate::ui::sp(10.))
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .hover(|s| s.bg(theme::hover()))
            .child(
                div()
                    .flex_none()
                    .w(px(18.))
                    .flex()
                    .justify_center()
                    .child(lead),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_MD))
                            .truncate()
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .truncate()
                            .child(facts.join(" · ")),
                    ),
            )
            .when(has_session, |d| {
                d.child(
                    ui::button(("auto-last", i), "Last run", ButtonKind::Ghost, true).on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(AutomationsEvent::OpenSession(i))),
                    ),
                )
            })
            .child(
                icon_button(("auto-run", i), "play", 14.)
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(AutomationsEvent::Run(i)))),
            )
            .child(
                icon_button(("auto-edit", i), "pencil-edit02", 14.).on_click(
                    cx.listener(move |this, _, window, cx| this.edit(Some(i), window, cx)),
                ),
            )
            .child(
                icon_button(("auto-delete", i), "delete02", 14.)
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(AutomationsEvent::Delete(i)))),
            )
            .when(scheduled, |d| {
                let on = a.enabled;
                d.child(
                    div()
                        .id(("auto-enabled", i))
                        .cursor_pointer()
                        .child(crate::settings_view::switch(on))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(AutomationsEvent::SetEnabled(i, !on))
                        })),
                )
            })
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let now = unix_now();
        let mut list = div()
            .id("auto-list")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(2.))
            .p(crate::ui::sp(8.))
            .min_h_0()
            .overflow_y_scroll();
        if !self.rules.is_empty() {
            list = list.child(
                div()
                    .px(crate::ui::sp(10.))
                    .pt(crate::ui::sp(4.))
                    .pb(crate::ui::sp(2.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child("Waiting for something · made in the palette"),
            );
            for r in &self.rules {
                list = list.child(self.render_rule(r, now, cx));
            }
            if !self.items.is_empty() {
                list = list.child(
                    div()
                        .px(crate::ui::sp(10.))
                        .pt(crate::ui::sp(10.))
                        .pb(crate::ui::sp(2.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child("Saved"),
                );
            }
        }
        if self.items.is_empty() && self.rules.is_empty() {
            list = list.child(
                ui::empty_state(
                    "flash",
                    "No automations yet",
                    "Save a shell command or an agent prompt, run it with one click, or let Arakun run it on a schedule while it is open.",
                    None,
                ),
            );
        }
        for (i, a) in self.items.iter().enumerate() {
            list = list.child(self.render_row(i, a, now, cx));
        }
        list
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let creating = self.editing == Some(None);
        let mut body = div()
            .id("auto-editor")
            .flex()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .p(crate::ui::sp(16.))
            .min_h_0()
            .overflow_y_scroll();
        if creating {
            let mut chips = div().flex().flex_wrap().gap(crate::ui::sp(6.));
            for (ti, t) in TEMPLATES.iter().enumerate() {
                chips = chips.child(
                    div()
                        .id(("auto-template", ti))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(ui::CONTROL_H))
                        .px(crate::ui::sp(10.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .child(match t.agent {
                            Some(agent) => crate::shell::agent_icon(agent, 12.),
                            None => icon("flash", 12.)
                                .text_color(theme::text_subtle())
                                .into_any_element(),
                        })
                        .child(t.name)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let t = &TEMPLATES[ti];
                            this.load(t.name, t.agent, t.body, t.schedule, cx);
                            this.error = None;
                            cx.notify();
                        })),
                );
            }
            body = body.child(field("Start from a template", chips));
        }
        body = body
            .child(field("Name", field_box().child(self.name.clone())))
            .child(field(
                "Runs",
                segmented(
                    "auto-kind",
                    &[(true, "Agent prompt"), (false, "Shell command")],
                    self.agent.is_some(),
                    cx,
                    |this, agent, cx| {
                        this.agent = agent.then_some(this.agent.unwrap_or(Agent::Droid));
                        cx.notify();
                    },
                ),
            ));
        if let Some(agent) = self.agent {
            body = body.child(field(
                "Agent",
                segmented(
                    "auto-agent",
                    &[
                        (Agent::Droid, Agent::Droid.label()),
                        (Agent::Claude, Agent::Claude.label()),
                        (Agent::OpenCode, Agent::OpenCode.label()),
                    ],
                    agent,
                    cx,
                    |this, agent, cx| {
                        this.agent = Some(agent);
                        cx.notify();
                    },
                ),
            ));
        }
        body = body.child(field(
            if self.agent.is_some() {
                "Prompt"
            } else {
                "Command"
            },
            field_box().child(self.body.clone()),
        ));
        if self.envs.len() > 1 {
            let options: Vec<(Option<PathBuf>, SharedString)> = self
                .envs
                .iter()
                .enumerate()
                .map(|(i, (p, n))| (if i == 0 { None } else { Some(p.clone()) }, n.clone()))
                .collect();
            let mut row = div().flex().flex_wrap().gap(crate::ui::sp(4.));
            for (oi, (value, label)) in options.into_iter().enumerate() {
                let selected = value == self.env;
                row = row.child(
                    ui::chip(("auto-env", oi), label, selected).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.env = value.clone();
                            cx.notify();
                        },
                    )),
                );
            }
            body = body.child(field("Environment", row));
        }
        body = body.child(field(
            "Schedule",
            segmented(
                "auto-every",
                &[
                    (Every::Manual, "Manual"),
                    (Every::Hourly, "Hourly"),
                    (Every::Daily, "Daily"),
                    (Every::Weekdays, "Weekdays"),
                    (Every::Weekly, "Weekly"),
                ],
                self.every,
                cx,
                |this, every, cx| this.set_every(every, cx),
            ),
        ));
        if self.every == Every::Weekly {
            let mut days = div().flex().flex_wrap().gap(crate::ui::sp(4.));
            for d in [1u8, 2, 3, 4, 5, 6, 0] {
                days = days.child(
                    ui::chip(
                        ("auto-day", d as usize),
                        &schedule::WEEKDAYS[d as usize][..3],
                        self.day == d,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.day = d;
                        cx.notify();
                    })),
                );
            }
            body = body.child(field("Day", days));
        }
        if self.every != Every::Manual {
            let preview = match self.schedule(cx) {
                Ok(s) => s
                    .next_after(unix_now())
                    .map(|n| format!("Next run {}", schedule::when(n, unix_now())))
                    .unwrap_or_default(),
                Err(e) => e.to_string(),
            };
            body = body.child(field(
                if self.every == Every::Hourly {
                    "Minute past the hour"
                } else {
                    "Time"
                },
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .child(field_box().w(px(96.)).child(self.time.clone()))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_subtle())
                            .child(preview),
                    ),
            ));
            body = body.child(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .line_height(px(16.))
                    .text_color(theme::text_faint())
                    .child(
                        "Scheduled runs happen while Arakun is open; one missed by less than an hour runs \
                         when Arakun notices. Agent runs show up in Sessions and the Inbox.",
                    ),
            );
        }
        body.when_some(self.error.clone(), |d, e| {
            d.child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::error())
                    .child(e),
            )
        })
    }
}

fn field(label: &str, content: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(6.))
        .child(
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_muted())
                .child(label.to_string()),
        )
        .child(content)
}

fn field_box() -> gpui::Div {
    ui::field()
}

fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    selected: T,
    cx: &mut Context<AutomationsDialog>,
    on: impl Fn(&mut AutomationsDialog, T, &mut Context<AutomationsDialog>) + Clone + 'static,
) -> gpui::Div {
    let mut row = ui::segmented().self_start();
    for (i, (value, label)) in options.iter().copied().enumerate() {
        let on = on.clone();
        row = row.child(
            ui::segment((id, i), None, label, value == selected)
                .on_click(cx.listener(move |this, _, _, cx| on(this, value, cx))),
        );
    }
    row
}

impl Focusable for AutomationsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AutomationsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.editing.is_some();
        let (title, description): (SharedString, SharedString) = match self.editing {
            None => (
                "Automations".into(),
                format!("{} · run by hand or on a schedule", self.project).into(),
            ),
            Some(None) => ("New automation".into(), self.project.clone()),
            Some(Some(_)) => ("Edit automation".into(), self.project.clone()),
        };
        let body = if editing {
            self.render_editor(cx)
        } else {
            self.render_list(cx)
        };
        let footer = div()
            .flex()
            .flex_none()
            .justify_end()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(16.))
            .py(crate::ui::sp(12.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .when(editing, |d| {
                d.child(
                    ui::button("auto-cancel", "Back", ButtonKind::Ghost, true)
                        .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                )
                .child(
                    ui::button("auto-save", "Save", ButtonKind::Primary, true)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                )
            })
            .when(!editing, |d| {
                d.child(
                    ui::button("auto-new", "New automation", ButtonKind::Primary, true)
                        .on_click(cx.listener(|this, _, window, cx| this.edit(None, window, cx))),
                )
            });
        ui::modal_panel("automations", 560.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                // The text fields handle their own Escape (`InputEvent::Cancel`).
                if ev.keystroke.key == "escape" && this.focus.is_focused(window) {
                    this.back(window, cx);
                }
            }))
            .child(ui::modal_header(
                title,
                Some(description),
                ui::close_button("auto-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AutomationsEvent::Dismissed))),
            ))
            .child(body.flex_1().max_h(px(520.)))
            .child(footer)
    }
}

/// How many rules the dialog lists when some are finished.
const RULES_SHOWN: usize = 8;

/// Waiting rules first, newest first, then the last finished ones. Every
/// waiting or running rule stays, with its Cancel: only finished ones are
/// cut to fit.
fn shown_rules(mut rules: Vec<crate::palette::rules::Rule>) -> Vec<crate::palette::rules::Rule> {
    use crate::palette::rules::State;
    rules.sort_by_key(|r| (r.state != State::Waiting, std::cmp::Reverse(r.created)));
    let live = rules
        .iter()
        .filter(|r| matches!(r.state, State::Waiting | State::Running))
        .count();
    let mut finished = RULES_SHOWN.saturating_sub(live);
    rules.retain(|r| {
        if matches!(r.state, State::Waiting | State::Running) {
            return true;
        }
        let keep = finished > 0;
        finished = finished.saturating_sub(1);
        keep
    });
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::rules::{Rule, State, Trigger};

    fn rule(id: u64, state: State) -> Rule {
        Rule {
            id,
            title: String::new(),
            trigger: Trigger::CiFailed {
                branch: "main".into(),
                project: None,
            },
            steps: Vec::new(),
            dir: None,
            created: id,
            expires: 0,
            state,
            note: String::new(),
            seen_busy: false,
        }
    }

    #[test]
    fn every_waiting_rule_is_listed() {
        let mut rules: Vec<Rule> = (0..10).map(|i| rule(i, State::Done)).collect();
        rules.extend((10..20).map(|i| rule(i, State::Waiting)));
        rules.push(rule(20, State::Running));
        let shown = shown_rules(rules);
        assert_eq!(shown.len(), 11);
        assert!(shown.iter().all(|r| r.state != State::Done));
        // With few waiting, the newest finished fill the rest.
        let mut rules: Vec<Rule> = (0..10).map(|i| rule(i, State::Done)).collect();
        rules.push(rule(10, State::Waiting));
        let ids: Vec<u64> = shown_rules(rules).iter().map(|r| r.id).collect();
        assert_eq!(ids, [10, 9, 8, 7, 6, 5, 4, 3]);
    }
}
