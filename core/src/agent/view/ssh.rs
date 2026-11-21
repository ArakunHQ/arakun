//! A host key and a command approval belong to one local view, not to the agent.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::agent::session_ssh::{self, HostKey, Target};
use crate::agent::ssh_request;
use crate::ui::{self, ButtonKind};
use gpui::Stateful;

const HISTORY: usize = 5;

#[derive(Default)]
pub(super) struct State {
    sheet: Option<Sheet>,
    // Clicks carry the revision of the controls that were actually on screen.
    revision: u64,
}

impl State {
    pub(super) fn sync(&mut self, thread: &Thread, reset: bool) {
        if self.sheet.as_ref().is_some_and(|s| {
            reset || !local_session(thread) || s.session.as_deref() != thread.session_id()
        }) {
            self.sheet = None;
            self.revision += 1;
        }
    }

    pub(super) fn shown(&self) -> bool {
        self.sheet.as_ref().is_some_and(|s| s.shown)
    }
}

struct Sheet {
    session: Option<String>,
    shown: bool,
    destination: Entity<TextInput>,
    port: Entity<TextInput>,
    draft: Entity<TextInput>,
    _subs: Vec<Subscription>,
    phase: Phase,
    error: Option<String>,
    worker: Option<Worker>,
}

// Only the sheet owns the worker. The task holds a copy of its flag, so
// dropping the view or disconnecting can cancel a blocking child immediately.
#[derive(Default)]
struct Worker(Arc<AtomicBool>);

impl Worker {
    fn accepts(&self, token: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(&self.0, token) && !token.load(Ordering::Acquire)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
struct Pin {
    target: Arc<Target>,
    key: HostKey,
}

enum Phase {
    Destination,
    Probe(Arc<Target>),
    Verify { pin: Pin, matched: bool },
    Check(Pin),
    Connected(Connection),
}

struct Connection {
    pin: Pin,
    // Entries before the pinned check finished (or already acted on) are ineligible.
    after: usize,
    displayed: Option<(usize, String)>,
    told: bool,
    running: Option<String>,
    recent: VecDeque<Record>,
}

struct Record {
    command: String,
    ok: bool,
    output: String,
    shared: bool,
}

#[derive(Clone)]
enum Action {
    Open(Option<String>),
    Toggle,
    Disconnect,
    Probe,
    EditHost,
    Match,
    Trust,
    Tell,
    Run {
        entry: Option<usize>,
        command: String,
    },
    Share,
}

fn proposal(entries: &[Entry], after: usize) -> Option<(usize, String)> {
    for (offset, entry) in entries.get(after..)?.iter().enumerate().rev() {
        match entry {
            Entry::Agent { text } => {
                return ssh_request::proposal(text).map(|cmd| (after + offset, cmd));
            }
            // A new user turn supersedes the previous turn's proposal.
            Entry::User { .. } => return None,
            _ => {}
        }
    }
    None
}

fn local_session(thread: &Thread) -> bool {
    !thread.is_remote() && !thread.is_subagent_view() && !thread.is_confined()
}

fn agent_ready(thread: &Thread) -> bool {
    local_session(thread)
        && thread.session_id().is_some()
        && matches!(thread.status(), ThreadStatus::Idle)
}

impl ThreadView {
    fn open_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.thread.read(cx).session_id().map(str::to_owned);
        let destination = cx.new(|cx| TextInput::new("user@host", cx).mono());
        let port = cx.new(|cx| {
            let mut input = TextInput::new("22", cx).mono();
            input.set_text("22", cx);
            input
        });
        let draft = cx.new(|cx| TextInput::new("Enter a single-line command", cx).mono());
        let subs = [&destination, &port, &draft]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| {
                    match ev {
                        InputEvent::Changed => {
                            this.ssh.revision += 1;
                            cx.notify();
                        }
                        InputEvent::Cancel if this.ssh.shown() => {
                            this.ssh_action(Action::Toggle, window, cx);
                        }
                        // Enter in an editor is not consent to connect or execute.
                        _ => {}
                    }
                })
            })
            .collect();
        window.focus(&destination.focus_handle(cx), cx);
        self.clear_selection();
        self.ssh.sheet = Some(Sheet {
            session,
            shown: true,
            destination,
            port,
            draft,
            _subs: subs,
            phase: Phase::Destination,
            error: None,
            worker: None,
        });
    }

    fn ssh_work<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&mut Sheet, Result<T, String>, &Thread) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(sheet) = &mut self.ssh.sheet else {
            return;
        };
        debug_assert!(sheet.worker.is_none());
        let worker = Worker::default();
        let cancel = worker.0.clone();
        sheet.worker = Some(worker);
        sheet.error = None;
        cx.spawn(async move |this, cx| {
            let flag = cancel.clone();
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    if flag.load(Ordering::Acquire) {
                        return Err("Cancelled".into());
                    }
                    work(&flag)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let thread = this.thread.read(cx);
                this.ssh.sync(thread, false);
                if let Some(sheet) = &mut this.ssh.sheet
                    && sheet.worker.as_ref().is_some_and(|w| w.accepts(&cancel))
                {
                    sheet.worker = None;
                    done(sheet, result, thread);
                    this.ssh.revision += 1;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ssh_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        self.ssh.sync(self.thread.read(cx), false);
        let thread = self.thread.read(cx);
        // A project that wants the sandbox, or whose Git folder can't be
        // trusted to say, gets no SSH: fail closed.
        let sandboxed = !matches!(crate::agent::sandbox::wanted(thread.cwd()), Ok(false));
        if !local_session(thread) || sandboxed {
            self.ssh.sheet = None;
            return;
        }
        self.ssh.revision += 1;
        cx.notify();
        if let Action::Open(session) = action {
            if self.ssh.sheet.is_none() && session.as_deref() == self.thread.read(cx).session_id() {
                self.open_ssh(window, cx);
            }
            return;
        }
        let Some(sheet) = &mut self.ssh.sheet else {
            return;
        };
        match action {
            Action::Open(_) => unreachable!(),
            Action::Disconnect => {
                self.ssh.sheet = None;
                window.focus(&self.focus, cx);
            }
            Action::Toggle => {
                sheet.shown = !sheet.shown;
                window.focus(&self.focus, cx);
            }
            _ if !sheet.shown => {}
            Action::Probe if matches!(sheet.phase, Phase::Destination) => {
                let target = match Target::parse(
                    sheet.destination.read(cx).text(),
                    sheet.port.read(cx).text(),
                ) {
                    Ok(target) => Arc::new(target),
                    Err(error) => {
                        sheet.error = Some(error);
                        return;
                    }
                };
                sheet.phase = Phase::Probe(target.clone());
                let host = target.clone();
                self.ssh_work(
                    move |cancel| session_ssh::probe(&host, cancel),
                    move |sheet, result, _| match result {
                        Ok(key) => {
                            sheet.phase = Phase::Verify {
                                pin: Pin { target, key },
                                matched: false,
                            }
                        }
                        Err(error) => {
                            sheet.phase = Phase::Destination;
                            sheet.error = Some(error);
                        }
                    },
                    cx,
                );
            }
            Action::EditHost if matches!(sheet.phase, Phase::Verify { .. }) => {
                sheet.phase = Phase::Destination;
                sheet.error = None;
                window.focus(&sheet.destination.focus_handle(cx), cx);
            }
            Action::Match => {
                if let Phase::Verify { matched, .. } = &mut sheet.phase {
                    *matched = !*matched;
                }
            }
            Action::Trust => {
                let Phase::Verify { pin, matched: true } = &sheet.phase else {
                    return;
                };
                let pin = pin.clone();
                let check = pin.clone();
                sheet.phase = Phase::Check(pin.clone());
                self.ssh_work(
                    move |cancel| session_ssh::check(&check.target, &check.key, cancel),
                    move |sheet, result, thread| match result {
                        Ok(()) => {
                            sheet.phase = Phase::Connected(Connection {
                                pin,
                                after: thread.entries().len(),
                                displayed: None,
                                told: false,
                                running: None,
                                recent: VecDeque::new(),
                            });
                        }
                        Err(error) => {
                            sheet.error = Some(error);
                            sheet.phase = Phase::Verify {
                                pin,
                                matched: false,
                            };
                        }
                    },
                    cx,
                );
            }
            Action::Run { entry, command } => {
                let Phase::Connected(conn) = &mut sheet.phase else {
                    return;
                };
                if conn.running.is_some() || session_ssh::validate_command(&command).is_err() {
                    return;
                }
                if let Some(ix) = entry {
                    let t = self.thread.read(cx);
                    if t.is_busy()
                        || proposal(t.entries(), conn.after) != Some((ix, command.clone()))
                    {
                        return;
                    }
                    conn.after = ix + 1;
                } else {
                    if sheet.draft.read(cx).text() != command {
                        return;
                    }
                    sheet.draft.update(cx, |d, cx| d.clear(cx));
                }
                conn.running = Some(command.clone());
                let pin = conn.pin.clone();
                let exact = command.clone();
                self.ssh_work(
                    move |cancel| session_ssh::run(&pin.target, &pin.key, &exact, cancel),
                    move |sheet, result, _| {
                        let Phase::Connected(conn) = &mut sheet.phase else {
                            return;
                        };
                        conn.running = None;
                        let output = match result {
                            Ok(output) => output,
                            Err(error) => {
                                sheet.phase = Phase::Destination;
                                sheet.error = Some(format!("Connection closed: {error}"));
                                return;
                            }
                        };
                        conn.recent.push_front(Record {
                            command,
                            ok: output.success,
                            output: output.text,
                            shared: false,
                        });
                        conn.recent.truncate(HISTORY);
                    },
                    cx,
                );
            }
            Action::Tell | Action::Share => {
                let Phase::Connected(conn) = &mut sheet.phase else {
                    return;
                };
                let label = conn.pin.target.label();
                let share = matches!(action, Action::Share);
                let message = if share {
                    let Some(record) = conn.recent.front().filter(|r| !r.shared) else {
                        return;
                    };
                    ssh_request::result(&label, &record.command, record.ok, &record.output)
                } else if !conn.told {
                    ssh_request::instructions(&label)
                } else {
                    return;
                };
                let sent = self.thread.update(cx, |t, cx| {
                    if !agent_ready(t) || t.session_id() != sheet.session.as_deref() {
                        return false;
                    }
                    // A viewer or a session still applying settings may look idle.
                    // Never leave host data queued for a later agent connection.
                    let queued = t.queued().len();
                    let notes = t.take_notes();
                    t.send(message, cx);
                    for note in notes {
                        t.add_note(note);
                    }
                    if t.queued().len() > queued {
                        t.remove_queued(queued, cx);
                        return false;
                    }
                    true
                });
                if !sent {
                    sheet.error = Some("The agent is not ready. Nothing was shared.".into());
                } else {
                    sheet.error = None;
                    if share {
                        if let Some(last) = conn.recent.front_mut() {
                            last.shared = true;
                        }
                    } else {
                        conn.told = true;
                    }
                }
            }
            _ => {}
        }
    }
}

struct Controls<'a> {
    revision: u64,
    cx: &'a Context<'a, ThreadView>,
}

impl Controls<'_> {
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: ButtonKind,
        action: Option<Action>,
    ) -> AnyElement {
        self.clickable(
            ui::button((id, self.revision), label, kind, action.is_some()),
            action,
        )
    }

    /// A button that stays pressed while `on`: a check mark shows it, and
    /// screen readers say it.
    fn toggle(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        action: Action,
    ) -> AnyElement {
        let mark = on.then_some("checkmark-circle02");
        let button = ui::button_with((id, self.revision), mark, label, ButtonKind::Outline, true)
            .aria_toggled(ui::toggled(on));
        self.clickable(button, Some(action))
    }

    fn clickable(&self, button: Stateful<Div>, action: Option<Action>) -> AnyElement {
        let rev = self.revision;
        button
            .when_some(action, |d, action| {
                d.on_click(self.cx.listener(move |view, _, window, cx| {
                    if view.ssh.revision == rev {
                        view.ssh_action(action.clone(), window, cx);
                    }
                }))
            })
            .into_any_element()
    }
}

fn column() -> Div {
    div().flex().flex_col().min_w_0().gap(ui::sp(8.))
}

fn row() -> Div {
    div().flex().flex_wrap().items_center().gap(ui::sp(8.))
}

fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(ui::TEXT_SM))
        .text_color(theme::text_subtle())
        .child(text.into())
}

fn label(text: &'static str) -> Div {
    div()
        .text_size(px(ui::TEXT_SM))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

fn card() -> Div {
    column()
        .gap(ui::sp(12.))
        .p(ui::sp(16.))
        .rounded(ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
}

// Unlike the single-line editor, the approval surface wraps even an unbroken token.
// It has no line clamp, horizontal clipping, syntax substitutions or redaction.
fn literal(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .min_w_0()
        .whitespace_normal()
        .font_family(theme::FONT_MONO)
        .font_features(gpui::FontFeatures::disable_ligatures())
        .text_size(px(ui::TEXT_MD))
        .child(text.into())
}

fn code(text: &str) -> Div {
    div()
        .w_full()
        .min_w_0()
        .p(ui::sp(10.))
        .rounded(ui::rad(ui::RADIUS))
        .bg(theme::background())
        .child(literal(text.to_owned()))
}

fn heading(step: &'static str, title: &'static str, detail: &'static str) -> Div {
    column()
        .gap(ui::sp(4.))
        .child(
            div()
                .text_size(px(ui::TEXT_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_subtle())
                .child(step),
        )
        .child(
            div()
                .text_size(px(ui::TEXT_TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(caption(detail))
}

const STEP_HOST: &str = "SESSION SSH  ·  1 OF 3";
const STEP_KEY: &str = "SESSION SSH  ·  2 OF 3";
const STEP_LOGIN: &str = "SESSION SSH  ·  3 OF 3";
const STEP_PINNED: &str = "SESSION SSH  ·  KEY PINNED";
const FINGERPRINT: &str = "ED25519 · SHA256 fingerprint";
const DESTINATION_DETAIL: &str =
    "Use a literal user@host. The first step only reads its public host key.";
const DESTINATION_NOTE: &str = "Login uses an unlocked SSH agent, not a password. \
    The host is attached only to this session.";
const PROBE_DETAIL: &str =
    "Waiting for the host to return its ED25519 public key. You can hide this panel.";
const VERIFY_DETAIL: &str = "Trust applies to this session only. \
    Arakun will check the pinned key before running a command.";
const COMPARE: &str = "Compare the entire fingerprint with a trusted source outside this SSH \
    connection, such as the host console or its administrator. If it differs, cancel setup.";
const CHECK_DETAIL: &str = "Testing noninteractive login with the key you verified.";
const CONNECTED_DETAIL: &str = "One approval per command. Each starts in a fresh shell, \
    with a 10-minute limit and up to 64 KiB of output.";

fn destination(body: Div, sheet: &Sheet, controls: &Controls, cx: &App) -> Div {
    let field = |name, input: &Entity<TextInput>| {
        column()
            .child(label(name))
            .child(ui::field().child(input.clone()))
    };
    let filled =
        !sheet.destination.read(cx).text().is_empty() && !sheet.port.read(cx).text().is_empty();
    let fields = div()
        .flex()
        .items_end()
        .gap(ui::sp(10.))
        .child(field("Destination", &sheet.destination).flex_1())
        .child(field("Port", &sheet.port).w(px(80.)));
    let probe = controls.button(
        "ssh-probe",
        "Probe host key",
        ButtonKind::Primary,
        filled.then_some(Action::Probe),
    );
    body.child(heading(STEP_HOST, "Connect a host", DESTINATION_DETAIL))
        .child(card().child(fields).child(row().child(probe)))
        .child(caption(DESTINATION_NOTE))
}

fn verify(body: Div, pin: &Pin, matched: bool, controls: &Controls) -> Div {
    let fingerprint = &pin.key.fingerprint;
    let title = row()
        .child(icon("square-lock02", 16.).text_color(theme::needs_you()))
        .child(label(FINGERPRINT))
        .child(markdown::copy_button(
            "ssh-copy-fingerprint",
            fingerprint.clone(),
        ));
    let checked = if matched {
        "Fingerprint verified"
    } else {
        "I verified the fingerprint"
    };
    let actions = row()
        .child(controls.button(
            "ssh-trust",
            "Trust & connect",
            ButtonKind::Primary,
            matched.then_some(Action::Trust),
        ))
        .child(controls.button(
            "ssh-edit",
            "Change host",
            ButtonKind::Ghost,
            Some(Action::EditHost),
        ));
    body.child(heading(STEP_KEY, "Verify the host", VERIFY_DETAIL))
        .child(literal(pin.target.label()))
        .child(
            card()
                .child(title)
                .child(code(fingerprint))
                .child(caption(COMPARE))
                .child(row().child(controls.toggle("ssh-match", checked, matched, Action::Match)))
                .child(actions),
        )
}

fn connected(
    body: Div,
    sheet: &Sheet,
    conn: &Connection,
    ready: bool,
    controls: &Controls,
    cx: &App,
) -> Div {
    let idle = conn.running.is_none();
    let tell = controls.button(
        "ssh-tell",
        if conn.told {
            "Agent informed"
        } else {
            "Tell agent"
        },
        ButtonKind::Outline,
        (ready && !conn.told).then_some(Action::Tell),
    );
    let mut body = body
        .child(heading(STEP_PINNED, "Remote commands", CONNECTED_DETAIL))
        .child(literal(conn.pin.target.label()))
        .child(
            row()
                .child(tell)
                .child(caption("Share the host name and how to propose a command.")),
        );
    if let Some((entry, command)) = &conn.displayed {
        let run = idle.then(|| Action::Run {
            entry: Some(*entry),
            command: command.clone(),
        });
        body = body.child(
            card()
                .child(
                    row()
                        .child(icon("bot", 16.).text_color(theme::needs_you()))
                        .child(label("Agent proposal")),
                )
                .child(code(command))
                .child(row().child(controls.button(
                    "ssh-run-agent",
                    "Run once",
                    ButtonKind::Primary,
                    run,
                ))),
        );
    }
    let command = sheet.draft.read(cx).text();
    let valid = session_ssh::validate_command(command);
    let mut manual = card()
        .child(label("Your command"))
        .child(ui::field().child(sheet.draft.clone()));
    if !command.is_empty() {
        manual = match &valid {
            Ok(()) => manual
                .child(caption("Exact command to run"))
                .child(code(command)),
            Err(reason) => manual.child(caption(reason.clone()).text_color(theme::needs_you())),
        };
    }
    let run = (valid.is_ok() && idle).then(|| Action::Run {
        entry: None,
        command: command.to_owned(),
    });
    body = body.child(manual.child(row().child(controls.button(
        "ssh-run-draft",
        "Run once",
        ButtonKind::Outline,
        run,
    ))));
    if let Some(command) = &conn.running {
        body = body.child(card().child(label("Running")).child(code(command)));
    }
    if let Some(record) = conn.recent.front() {
        body = body.child(last_result(record, ready, controls));
    }
    if conn.recent.len() > 1 {
        let earlier = conn.recent.iter().skip(1).map(|record| {
            let mark = if record.ok {
                "checkmark-circle02"
            } else {
                "alert-circle"
            };
            row()
                .child(icon(mark, 13.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(literal(record.command.clone())),
                )
        });
        body = body.child(column().child(label("Earlier commands")).children(earlier));
    }
    body
}

fn last_result(record: &Record, ready: bool, controls: &Controls) -> Div {
    let output = if record.output.is_empty() {
        "(no output)"
    } else {
        &record.output
    };
    let (verdict, tone) = if record.ok {
        ("Succeeded", theme::success())
    } else {
        ("Failed", theme::error())
    };
    let share = controls.button(
        "ssh-share",
        if record.shared {
            "Result shared"
        } else {
            "Share result with this session"
        },
        ButtonKind::Outline,
        (ready && !record.shared).then_some(Action::Share),
    );
    let note = if ready || record.shared {
        "Only the displayed command and its result are shared with the host name."
    } else {
        "Wait for the local agent to be idle before sharing."
    };
    card()
        .child(
            row()
                .child(label("Last result"))
                .child(caption(verdict).text_color(tone)),
        )
        .child(code(&record.command))
        .child(
            div()
                .id("ssh-output")
                .max_h(px(180.))
                .overflow_y_scroll()
                .child(literal(output.to_owned())),
        )
        .child(row().child(share))
        .child(caption(note))
}

impl ThreadView {
    pub(super) fn render_ssh(
        &mut self,
        cx: &Context<Self>,
    ) -> (Option<AnyElement>, Option<AnyElement>) {
        let thread = self.thread.read(cx);
        self.ssh.sync(thread, false);
        if !local_session(thread) {
            return (None, None);
        }
        if let Some(sheet) = &mut self.ssh.sheet
            && let Phase::Connected(conn) = &mut sheet.phase
        {
            let current = (!thread.is_busy())
                .then(|| proposal(thread.entries(), conn.after))
                .flatten();
            if conn.displayed != current {
                conn.displayed = current;
                // GPUI pairs mouse-down and mouse-up by element ID. A proposal
                // replaced between the two needs a fresh button, not just new text.
                self.ssh.revision += 1;
            }
        }
        let controls = Controls {
            revision: self.ssh.revision,
            cx,
        };
        let bar = div()
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(ui::sp(6.))
            .px(ui::sp(10.))
            .py(ui::sp(2.));
        let Some(sheet) = &self.ssh.sheet else {
            return (
                Some(
                    bar.justify_end()
                        .child(controls.button(
                            "ssh-open",
                            "Connect SSH",
                            ButtonKind::Ghost,
                            Some(Action::Open(thread.session_id().map(str::to_owned))),
                        ))
                        .into_any_element(),
                ),
                None,
            );
        };
        let (host, status, color) = match &sheet.phase {
            Phase::Destination => ("Session SSH".into(), "Setup", theme::text_subtle()),
            Phase::Probe(t) => (t.label(), "Reading host key", theme::text_subtle()),
            Phase::Verify { pin, .. } => (pin.target.label(), "Verify key", theme::needs_you()),
            Phase::Check(pin) => (pin.target.label(), "Checking login", theme::text_subtle()),
            Phase::Connected(conn) => {
                let (status, color) = if conn.running.is_some() {
                    ("Running", theme::text_subtle())
                } else if conn.displayed.is_some() {
                    ("Needs review", theme::needs_you())
                } else if let Some(last) = conn.recent.front().filter(|r| !r.shared) {
                    if last.ok {
                        ("Result ready", theme::success().into())
                    } else {
                        ("Command failed", theme::error().into())
                    }
                } else {
                    ("Connected", theme::success().into())
                };
                (conn.pin.target.label(), status, color)
            }
        };
        let (status, color) = if sheet.error.is_some() {
            ("Needs attention", theme::needs_you())
        } else {
            (status, color)
        };
        let status_icon = if sheet.worker.is_some() {
            ui::spinner("ssh-working", 14., color).into_any_element()
        } else {
            icon("computer-terminal01", 15.)
                .text_color(color)
                .into_any_element()
        };
        let bar = bar
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(status_icon)
            .child(
                div()
                    .flex_none()
                    .text_color(color)
                    .text_size(px(ui::TEXT_SM))
                    .child(status),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(host.clone()),
            )
            .child(controls.button(
                "ssh-visible",
                if sheet.shown { "Hide SSH" } else { "Show SSH" },
                ButtonKind::Ghost,
                Some(Action::Toggle),
            ))
            .child(controls.button(
                "ssh-end",
                if matches!(sheet.phase, Phase::Connected(_)) {
                    "Disconnect"
                } else {
                    "Cancel setup"
                },
                ButtonKind::Outline,
                Some(Action::Disconnect),
            ))
            .into_any_element();
        if !sheet.shown {
            return (Some(bar), None);
        }
        let body = column()
            .w_full()
            .max_w(px(720.))
            .mx_auto()
            .p(ui::sp(20.))
            .gap(ui::sp(16.));
        let mut body = match &sheet.phase {
            Phase::Destination => destination(body, sheet, &controls, cx),
            Phase::Probe(_) => body
                .child(heading(STEP_HOST, "Reading host key", PROBE_DETAIL))
                .child(code(&host)),
            Phase::Verify { pin, matched } => verify(body, pin, *matched, &controls),
            Phase::Check(pin) => body
                .child(heading(STEP_LOGIN, "Checking the pinned key", CHECK_DETAIL))
                .child(
                    card()
                        .child(literal(host))
                        .child(caption(FINGERPRINT))
                        .child(code(&pin.key.fingerprint)),
                ),
            Phase::Connected(conn) => {
                connected(body, sheet, conn, agent_ready(thread), &controls, cx)
            }
        };
        if let Some(error) = &sheet.error {
            body = body.child(
                card()
                    .border_color(theme::needs_you())
                    .child(label("SSH notice"))
                    .child(caption(error.clone())),
            );
        }
        let scroll = div()
            .id("ssh-sheet")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .child(body)
            .into_any_element();
        (Some(bar), Some(scroll))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(text: &str) -> Entry {
        Entry::Agent { text: text.into() }
    }

    #[test]
    fn only_the_latest_post_connection_reply_can_propose() {
        let entries = [
            reply("```arakun-ssh\nhostname\n```"),
            reply("No command needed."),
        ];
        assert_eq!(proposal(&entries[..1], 1), None);
        assert_eq!(proposal(&entries, 0), None);
        assert_eq!(proposal(&entries[..1], 0), Some((0, "hostname".into())));
    }

    #[test]
    fn shared_remote_data_cannot_attach_a_local_file() {
        let output = "see @local-image.png for details";
        let shared = ssh_request::result("me@host:22", "pwd", true, output);
        assert!(crate::agent::attach::mentions(&shared).is_empty());
        let (_, json) = shared.split_once('\n').unwrap();
        let record: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(record["output"], output);
        assert_eq!(record["host"], "me@host:22");
    }

    #[test]
    fn discarding_work_cancels_its_flag_and_excludes_its_late_reply() {
        let work = Worker::default();
        let flag = work.0.clone();
        assert!(work.accepts(&flag));
        drop(work);
        let replacement = Worker::default();
        assert!(flag.load(Ordering::Acquire));
        assert!(!replacement.accepts(&flag));
    }
}
