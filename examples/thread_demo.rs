//! Standalone harness for `src/agent`: one ACP thread in a window with a minimal composer.
//!
//!   cargo run --release --example thread_demo -- <claude|droid> <cwd> [options]
//!
//! Options:
//!   --prompt <text>     send this as soon as the window opens
//!   --mode <id>         session mode to start in
//!   --model <id>        model id to start with
//!   --resume <id>       load an existing ACP session
//!   --restore <path>    reopen a saved transcript
//!   --save <path>       write the transcript after every turn
//!
//! Enter sends (queued while the agent works), Escape stops the turn. Clicking the mode or model chip
//! cycles through the agent's choices.

#[path = "../src/agent/mod.rs"]
mod agent;
#[allow(dead_code)]
#[path = "../src/headless/broker.rs"]
mod broker;
#[path = "../src/durable.rs"]
mod durable;
#[path = "../src/fetch.rs"]
#[allow(dead_code)]
mod fetch;
#[allow(dead_code)]
#[path = "../src/headless/sandbox.rs"]
mod sandbox;
#[allow(dead_code)]
mod headless {
    // The agent's sandbox and broker; the session store they need is the app's, which examples don't run.
    pub(crate) use crate::{broker, sandbox};
    pub fn root() -> std::path::PathBuf {
        crate::platform::app_dir().join("headless")
    }
}
mod memory {
    pub fn prompt_for(_: &std::path::Path) -> String {
        String::new()
    }
}
#[path = "../src/assets.rs"]
mod assets;
#[path = "../src/caret.rs"]
mod caret;
#[allow(dead_code)]
#[path = "../src/highlight.rs"]
mod highlight;
#[allow(dead_code)]
#[path = "../src/input.rs"]
mod input;
#[allow(dead_code)]
#[path = "../src/mascot.rs"]
mod mascot;
#[allow(dead_code)]
#[path = "../src/platform.rs"]
mod platform;
#[allow(dead_code)]
#[path = "../src/redact.rs"]
mod redact;
#[allow(dead_code)]
#[path = "../src/schedule.rs"]
mod schedule;
#[allow(dead_code)]
#[path = "../src/theme.rs"]
mod theme;
#[allow(dead_code, unused_imports)]
#[path = "../src/ui.rs"]
mod ui;
#[path = "../src/zoom.rs"]
#[allow(dead_code)]
mod zoom;

use std::path::PathBuf;

use agent::{AgentKind, SessionMeta, Thread, ThreadEvent, ThreadStatus, ThreadView};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px,
    size,
};
use input::{InputEvent, TextInput};

struct Args {
    kind: AgentKind,
    cwd: PathBuf,
    prompt: Option<String>,
    mode: Option<String>,
    model: Option<String>,
    resume: Option<String>,
    restore: Option<PathBuf>,
    save: Option<PathBuf>,
}

fn parse_args() -> Args {
    let usage = || -> ! {
        eprintln!(
            "usage: thread_demo <claude|droid> <cwd> [--prompt t] [--mode m] [--model m] [--resume id] [--restore p] [--save p]"
        );
        std::process::exit(2)
    };
    let mut it = std::env::args().skip(1);
    let kind = it
        .next()
        .and_then(|k| AgentKind::parse(&k))
        .unwrap_or_else(|| usage());
    let cwd = it.next().map(PathBuf::from).unwrap_or_else(|| usage());
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let mut args = Args {
        kind,
        cwd,
        prompt: None,
        mode: None,
        model: None,
        resume: None,
        restore: None,
        save: None,
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--prompt" => args.prompt = Some(value()),
            "--mode" => args.mode = Some(value()),
            "--model" => args.model = Some(value()),
            "--resume" => args.resume = Some(value()),
            "--restore" => args.restore = Some(PathBuf::from(value())),
            "--save" => args.save = Some(PathBuf::from(value())),
            _ => usage(),
        }
    }
    args
}

struct Demo {
    thread: Entity<Thread>,
    view: Entity<ThreadView>,
    input: Entity<TextInput>,
    save: Option<PathBuf>,
    _subs: Vec<Subscription>,
}

impl Demo {
    fn new(args: Args, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let thread = match &args.restore {
            Some(path) => Thread::restore(path, cx).unwrap_or_else(|err| {
                eprintln!("thread_demo: {err:#}");
                std::process::exit(1)
            }),
            None => {
                let resume = args.resume.clone().map(|id| SessionMeta {
                    agent: args.kind,
                    session_id: Some(id),
                    title: String::new(),
                    cwd: args.cwd.clone(),
                    created: 0,
                    updated: 0,
                    pr: None,
                });
                Thread::start(args.kind, args.cwd.clone(), resume, cx)
            }
        };
        thread.update(cx, |t, cx| {
            if let Some(m) = &args.mode {
                t.set_mode(m, cx);
            }
            if let Some(m) = &args.model {
                t.set_model(m, cx);
            }
            if let Some(p) = &args.prompt {
                t.send(p.clone(), cx);
            }
        });
        let view = cx.new(|cx| ThreadView::new(thread.clone(), window, cx));
        let input = cx.new(|cx| TextInput::new("Ask, build, or describe a change…", cx));
        let subs = vec![
            cx.subscribe_in(&input, window, Self::input_event),
            cx.subscribe(&thread, |this, thread, ev, cx| {
                match ev {
                    ThreadEvent::Finished { stop_reason } => {
                        eprintln!("thread_demo: turn finished ({stop_reason})");
                        if let Some(path) = &this.save
                            && let Err(err) = thread.read(cx).save_transcript(path, cx)
                        {
                            eprintln!("thread_demo: save failed: {err:#}");
                        }
                    }
                    ThreadEvent::TitleChanged => {
                        eprintln!("thread_demo: title {}", thread.read(cx).title());
                    }
                    _ => {}
                }
                cx.notify();
            }),
        ];
        window.focus(&input.focus_handle(cx), cx);
        Demo {
            thread,
            view,
            input,
            save: args.save,
            _subs: subs,
        }
    }

    fn input_event(
        &mut self,
        input: &Entity<TextInput>,
        ev: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            InputEvent::Submit => {
                let text = input.read(cx).text().to_string();
                if text.trim().is_empty() {
                    return;
                }
                input.update(cx, |i, cx| i.set_text("", cx));
                self.thread.update(cx, |t, cx| t.send(text, cx));
            }
            InputEvent::Cancel => self.thread.update(cx, |t, cx| t.cancel(cx)),
            _ => {}
        }
    }

    fn cycle(&mut self, model: bool, cx: &mut Context<Self>) {
        self.thread.update(cx, |t, cx| {
            let Some((current, choices)) = (if model { t.models() } else { t.modes() }) else {
                return;
            };
            let Some(next) = choices
                .iter()
                .position(|c| c.id == current)
                .map(|i| &choices[(i + 1) % choices.len()])
                .or(choices.first())
            else {
                return;
            };
            let id = next.id.clone();
            if model {
                t.set_model(&id, cx);
            } else {
                t.set_mode(&id, cx);
            }
        });
        cx.notify();
    }

    fn chip(
        &self,
        id: &'static str,
        icon: &str,
        choice: Option<(String, Vec<agent::Choice>)>,
        model: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (current, choices) = choice?;
        let name = choices
            .iter()
            .find(|c| c.id == current)
            .map_or(current.clone(), |c| c.name.clone());
        Some(
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(5.))
                .h(px(24.))
                .px(px(8.))
                .rounded(px(6.))
                .text_size(px(12.))
                .text_color(theme::mix(0.6))
                .cursor_pointer()
                .hover(|s| s.bg(theme::mix(0.08)).text_color(theme::content()))
                .child(ui::icon(icon, 13.).text_color(theme::mix(0.5)))
                .child(SharedString::from(name))
                .on_click(cx.listener(move |this, _, _, cx| this.cycle(model, cx)))
                .into_any_element(),
        )
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.thread.read(cx);
        let kind = t.kind();
        let title = t.title();
        let status = t.status();
        let busy = t.is_busy() || t.is_cancelling();
        let (added, removed) = t.diff_stats();
        let footer_meta = t.usage().summary().unwrap_or_default();
        let modes = t.modes();
        let models = t.models();
        let status_label = match status {
            ThreadStatus::Connecting => "Connecting",
            ThreadStatus::Idle => "Idle",
            ThreadStatus::Working => "Working",
            ThreadStatus::NeedsInput => "Needs input",
            ThreadStatus::Error(_) => "Error",
        };

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .px(px(14.))
            .border_b_1()
            .border_color(theme::stroke())
            .text_size(px(13.))
            .child(kind.icon(14.))
            .child(div().flex_1().min_w_0().truncate().child(title))
            .when(added + removed > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .font_family(theme::FONT_MONO)
                        .text_size(px(12.))
                        .child(
                            div()
                                .text_color(theme::diff_add())
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_color(theme::diff_del())
                                .child(format!("-{removed}")),
                        ),
                )
            })
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::mix(0.5))
                    .child(status_label),
            );

        let send = div()
            .id("send")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(28.))
            .rounded_full()
            .cursor_pointer()
            .bg(theme::content())
            .hover(|s| s.bg(theme::mix(0.8)))
            .child(
                ui::icon(if busy { "square" } else { "arrow-up01" }, 14.)
                    .text_color(theme::background()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if busy && this.input.read(cx).text().trim().is_empty() {
                    this.thread.update(cx, |t, cx| t.cancel(cx));
                } else {
                    let ev = InputEvent::Submit;
                    let input = this.input.clone();
                    this.input_event(&input, &ev, window, cx);
                }
            }));

        let mode_chip = self.chip("mode", "shield-alert", modes, false, cx);
        let model_chip = self.chip("model", "sparkles", models, true, cx);
        let composer = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(896.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::mix(0.12))
            .bg(theme::mix(0.03))
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(12.))
                    .pt(px(12.))
                    .pb(px(6.))
                    .text_size(px(14.))
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .px(px(6.))
                    .pb(px(6.))
                    .children(mode_chip)
                    .children(model_chip)
                    .child(div().flex_1())
                    .when(!footer_meta.is_empty(), |d| {
                        d.child(
                            div()
                                .px(px(8.))
                                .text_size(px(11.))
                                .text_color(theme::mix(0.45))
                                .child(footer_meta),
                        )
                    })
                    .child(send),
            );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .text_color(theme::content())
            .font_family(theme::FONT_SANS)
            .child(header)
            .child(div().flex_1().min_h_0().child(self.view.clone()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .px(px(24.))
                    .pb(px(16.))
                    .pt(px(4.))
                    .child(composer),
            )
    }
}

fn main() {
    let args = parse_args();
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1100.), px(800.)), cx);
            let title = format!("arakun {} · {}", args.kind.label(), args.cwd.display());
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(title.into()),
                        ..Default::default()
                    }),
                    window_background: WindowBackgroundAppearance::Opaque,
                    app_id: Some("arakun-thread-demo".into()),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Demo::new(args, window, cx)),
            )
            .unwrap();
            cx.activate(true);
        });
}
