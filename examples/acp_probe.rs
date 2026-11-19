//! Headless ACP check: runs one prompt against an agent and prints the event stream.
//!
//!   cargo run --release --example acp_probe -- <claude|droid|opencode|codex|devin> <cwd> <prompt> [options]
//!
//! Options:
//!   --allow | --deny        answer permission requests (default: allow)
//!   --mode <id>             session mode (Droid: normal, auto-low, ...; Claude: default, acceptEdits, ...)
//!   --model <id>            model id from the agent's list
//!   --cancel-after <secs>   send session/cancel this long after the prompt
//!   --resume <session-id>   load an existing ACP session instead of creating one
//!   --save <path>           write the transcript JSON when the turn ends
//!   --restore <path>        reopen a saved transcript (and resume its session)
//!   --list                  print modes, models and config options once connected
//!   --then <secs> <prompt>  send another prompt this long after the first (may repeat)
//!
//! A turn that hands work to background subagents ends for the user before
//! they finish; the probe waits for them and for the agent's own follow-up.
//! Set ARAKUN_ACP_TRACE=1 to dump the raw JSON-RPC traffic on stderr.

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
#[path = "../src/guard.rs"]
mod guard;
#[path = "../src/rotate.rs"]
mod rotate;
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
#[path = "../src/frame_timer.rs"]
mod frame_timer;
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
#[allow(dead_code)]
#[path = "../src/ui.rs"]
mod ui;
#[path = "../src/zoom.rs"]
#[allow(dead_code)]
mod zoom;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agent::{AgentKind, Entry, SessionMeta, Thread, ThreadEvent, ThreadStatus, ToolStatus};
use gpui::{App, Entity};

struct Args {
    kind: AgentKind,
    cwd: PathBuf,
    prompt: String,
    allow: bool,
    mode: Option<String>,
    model: Option<String>,
    cancel_after: Option<f64>,
    resume: Option<String>,
    save: Option<PathBuf>,
    restore: Option<PathBuf>,
    list: bool,
    then: Vec<(f64, String)>,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let usage = || -> ! {
        eprintln!(
            "usage: acp_probe <claude|droid|opencode|codex|devin> <cwd> <prompt> [--allow|--deny] [--mode m] [--model m] [--cancel-after s] [--resume id] [--save p] [--restore p] [--list]"
        );
        std::process::exit(2)
    };
    let kind = it
        .next()
        .and_then(|k| AgentKind::parse(&k))
        .unwrap_or_else(|| usage());
    let cwd = it.next().map(PathBuf::from).unwrap_or_else(|| usage());
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let prompt = it.next().unwrap_or_else(|| usage());
    let mut a = Args {
        kind,
        cwd,
        prompt,
        allow: true,
        mode: None,
        model: None,
        cancel_after: None,
        resume: None,
        save: None,
        restore: None,
        list: false,
        then: Vec::new(),
    };
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--allow" => a.allow = true,
            "--deny" => a.allow = false,
            "--mode" => a.mode = it.next(),
            "--model" => a.model = it.next(),
            "--cancel-after" => a.cancel_after = it.next().and_then(|s| s.parse().ok()),
            "--resume" => a.resume = it.next(),
            "--save" => a.save = it.next().map(PathBuf::from),
            "--restore" => a.restore = it.next().map(PathBuf::from),
            "--list" => a.list = true,
            "--then" => {
                let secs = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
                let prompt = it.next().unwrap_or_else(|| usage());
                a.then.push((secs, prompt));
            }
            _ => usage(),
        }
    }
    a
}

#[derive(Default)]
struct Printer {
    printed: Vec<(u8, usize)>,
    tools: Vec<String>,
}

impl Printer {
    fn print(&mut self, entries: &[Entry], from: usize) {
        if entries.len() < self.printed.len() {
            self.printed.clear();
            self.tools.clear();
        }
        for (i, e) in entries
            .iter()
            .enumerate()
            .skip(from.min(self.printed.len()))
        {
            if self.printed.len() <= i {
                self.printed.push((0, 0));
                self.tools.push(String::new());
            }
            let (role, text) = match e {
                Entry::User { text, .. } => (1, text),
                Entry::Agent { text } => (2, text),
                Entry::Thought { text } => (3, text),
                Entry::Tool(t) => {
                    let sig = format!(
                        "[tool {} {:?}] {} {}{}",
                        t.kind,
                        t.status,
                        t.title,
                        if t.added + t.removed > 0 {
                            format!("+{} -{}", t.added, t.removed)
                        } else {
                            String::new()
                        },
                        if t.status == ToolStatus::Completed || t.status == ToolStatus::Failed {
                            format!(" ({} content blocks)", t.content.len())
                        } else {
                            String::new()
                        }
                    );
                    if self.tools[i] != sig {
                        println!("{sig}");
                        self.tools[i] = sig;
                    }
                    continue;
                }
                Entry::Plan { entries } => {
                    let sig = entries
                        .iter()
                        .map(|p| format!("{}:{}", p.status, p.content))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    if self.tools[i] != sig {
                        println!("[plan] {sig}");
                        self.tools[i] = sig;
                    }
                    continue;
                }
                Entry::Notice { text, error, .. } => {
                    if self.printed[i].1 == 0 {
                        println!("[notice{}] {text}", if *error { " error" } else { "" });
                        self.printed[i].1 = 1;
                    }
                    continue;
                }
                Entry::Compacted { .. } => {
                    if self.printed[i].1 == 0 {
                        println!("[compacted]");
                        self.printed[i].1 = 1;
                    }
                    continue;
                }
            };
            let (_, done) = self.printed[i];
            if done == 0 {
                let tag = match role {
                    1 => "\n[user] ",
                    2 => "\n[agent] ",
                    _ => "\n[thought] ",
                };
                print!("{tag}");
            }
            if text.len() > done {
                print!("{}", &text[done..]);
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            self.printed[i] = (role, text.len());
        }
    }
}

/// Prints how the session ended, saves it if asked, and exits.
fn report(
    thread: &Entity<Thread>,
    stop_reason: &str,
    save: Option<&PathBuf>,
    started: Instant,
    cx: &mut App,
) -> ! {
    let t = thread.read(cx);
    let (a, r) = t.diff_stats();
    let u = t.usage();
    println!(
        "\n[finished] stop={stop_reason} diff=+{a}/-{r} usage: ctx={:?}/{:?} cost={:?} in={} out={} ({:.1}s)",
        u.context_used,
        u.context_size,
        u.cost,
        u.input_tokens,
        u.output_tokens,
        started.elapsed().as_secs_f32()
    );
    println!(
        "[meta] {}",
        serde_json::to_string(&t.meta()).unwrap_or_default()
    );
    for s in t.subagents() {
        let c = s.thread.read(cx);
        println!(
            "[subagent] {} {:?} {:?} {}s entries={} tools={}",
            s.id,
            s.name,
            s.state,
            s.elapsed_secs(),
            c.entries().len(),
            c.entries()
                .iter()
                .filter(|e| matches!(e, Entry::Tool(_)))
                .count()
        );
    }
    if let Some(path) = save {
        match t.save_transcript(path, cx) {
            Ok(()) => println!("[saved] {}", path.display()),
            Err(e) => println!("[save failed] {e:#}"),
        }
    }
    let code = if stop_reason == "error" { 1 } else { 0 };
    finish(thread, code, cx);
}

fn finish(thread: &Entity<Thread>, code: i32, cx: &mut App) -> ! {
    thread.update(cx, |t, _| t.shutdown());
    // The process group gets SIGTERM from the connection drop; give it a beat.
    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(code)
}

fn main() {
    let args = Rc::new(parse_args());
    gpui_platform::headless().run(move |cx: &mut App| {
        let thread = match &args.restore {
            Some(path) => Thread::restore(path, cx).unwrap_or_else(|e| {
                eprintln!("restore failed: {e:#}");
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
                    seat: None,
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
        });
        let printer = Rc::new(RefCell::new(Printer::default()));
        let sent = Rc::new(Cell::new(false));
        let started = Instant::now();
        let last_status = Rc::new(RefCell::new(String::new()));

        // Print history that was already there (restore).
        printer.borrow_mut().print(thread.read(cx).entries(), 0);

        cx.subscribe(&thread, {
            let args = args.clone();
            move |thread, ev, cx| {
                match ev {
                    ThreadEvent::EntriesChanged { from } => {
                        printer.borrow_mut().print(thread.read(cx).entries(), *from)
                    }
                    ThreadEvent::StatusChanged => {
                        let t = thread.read(cx);
                        let s = format!("{:?}", t.status());
                        if *last_status.borrow() != s {
                            println!("\n[status] {s}  ({:.1}s)", started.elapsed().as_secs_f32());
                            *last_status.borrow_mut() = s;
                        }
                        match t.status() {
                            ThreadStatus::Idle if !sent.get() => {
                                sent.set(true);
                                println!(
                                    "[session] {} id={} load={} title={:?}",
                                    t.agent_title().unwrap_or("?"),
                                    t.session_id().unwrap_or("-"),
                                    t.supports_load(),
                                    t.title()
                                );
                                if args.list {
                                    if let Some((cur, modes)) = t.modes() {
                                        println!(
                                            "[modes] current={cur}: {}",
                                            modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ")
                                        );
                                    }
                                    if let Some((cur, models)) = t.models() {
                                        println!(
                                            "[models] current={cur}: {}",
                                            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ")
                                        );
                                    }
                                    for o in t.config_options() {
                                        println!(
                                            "[config] {} ({:?}) = {} [{} options]",
                                            o.id,
                                            o.category,
                                            o.current,
                                            o.options.len()
                                        );
                                    }
                                    println!("[commands] {}", t.commands().len());
                                }
                                let prompt = args.prompt.clone();
                                thread.update(cx, |t, cx| t.send(prompt, cx));
                                for (secs, prompt) in args.then.clone() {
                                    let thread = thread.clone();
                                    cx.spawn(async move |cx| {
                                        cx.background_executor().timer(Duration::from_secs_f64(secs)).await;
                                        println!("\n[probe] sending {prompt:?}");
                                        thread.update(cx, |t, cx| {
                                            println!("[probe] busy={} queued={}", t.is_busy(), t.queued().len());
                                            t.send(prompt, cx)
                                        });
                                    })
                                    .detach();
                                }
                                if let Some(secs) = args.cancel_after {
                                    let thread = thread.clone();
                                    cx.spawn(async move |cx| {
                                        cx.background_executor()
                                            .timer(Duration::from_secs_f64(secs))
                                            .await;
                                        println!("\n[probe] cancelling");
                                        thread.update(cx, |t, cx| t.cancel(cx));
                                    })
                                    .detach();
                                }
                            }
                            ThreadStatus::Error(e) => {
                                println!("[error] {e}");
                                let code = if sent.get() { 1 } else { 3 };
                                finish(&thread, code, cx);
                            }
                            _ => {}
                        }
                    }
                    ThreadEvent::NeedsPermission => {
                        let t = thread.read(cx);
                        if let Some(p) = t.pending_permission() {
                            println!(
                                "\n[permission] {} options: {}",
                                p.title,
                                p.options
                                    .iter()
                                    .map(|o| format!("{}={}({})", o.id, o.name, o.kind))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        let allow = args.allow;
                        println!("[probe] answering {}", if allow { "allow" } else { "deny" });
                        thread.update(cx, |t, cx| {
                            if allow {
                                t.allow_permission(cx)
                            } else {
                                t.reject_permission(cx)
                            }
                        });
                    }
                    ThreadEvent::TitleChanged => println!("\n[title] {}", thread.read(cx).title()),
                    ThreadEvent::Finished { stop_reason } => {
                        let t = thread.read(cx);
                        println!(
                            "\n[turn] stop={stop_reason} busy={} background={} running_subagents={} ({:.1}s)",
                            t.is_busy(),
                            t.in_background(),
                            t.running_subagents(),
                            started.elapsed().as_secs_f32()
                        );
                        // Background subagents, or prompts still to send: not done yet.
                        let pending = args.then.iter().any(|(secs, _)| started.elapsed().as_secs_f64() < *secs + 5.);
                        if t.in_background() || pending || !t.queued().is_empty() {
                            return;
                        }
                        report(&thread, stop_reason, args.save.as_ref(), started, cx);
                    }
                    ThreadEvent::Reset => printer.borrow_mut().print(thread.read(cx).entries(), 0),
                    ThreadEvent::SubagentsChanged { spawned } => {
                        if let Some(id) = spawned {
                            let s = thread.read(cx).subagent(id);
                            println!("\n[subagent spawned] {id} {:?}", s.map(|s| s.name.as_str()));
                            // Each subagent's own transcript, as it comes.
                            if let Some(child) = s.map(|s| s.thread.clone()) {
                                let id = id.clone();
                                cx.subscribe(&child, move |child, ev, cx| {
                                    if let ThreadEvent::EntriesChanged { from } = ev
                                        && let Some(e) = child.read(cx).entries().get(*from)
                                    {
                                        let line = match e {
                                            Entry::User { text, .. } => format!("user {:?}", text.chars().take(60).collect::<String>()),
                                            Entry::Agent { text } => format!("agent {text:?}"),
                                            Entry::Tool(t) => format!("tool {:?} {}", t.status, t.title),
                                            _ => "other".into(),
                                        };
                                        println!("[child {id}] #{from} {line}");
                                    }
                                })
                                .detach();
                            }
                        }
                        let t = thread.read(cx);
                        let states: Vec<String> =
                            t.subagents().iter().map(|s| format!("{}={:?}", s.id, s.state)).collect();
                        println!(
                            "[subagents] {} status={:?} busy={} background={} ({:.1}s)",
                            states.join(" "),
                            t.status(),
                            t.is_busy(),
                            t.in_background(),
                            started.elapsed().as_secs_f32()
                        );
                        // A held prompt answered with nothing more to say ends it all here.
                        let pending = args.then.iter().any(|(secs, _)| started.elapsed().as_secs_f64() < *secs + 5.);
                        if !t.in_background() && !t.is_busy() && sent.get() && !pending {
                            // The agent may still take a turn of its own about what finished.
                            println!("[probe] background done");
                            let (thread, save) = (thread.clone(), args.save.clone());
                            cx.spawn(async move |cx| {
                                cx.background_executor().timer(Duration::from_secs(20)).await;
                                cx.update(|cx| {
                                    let t = thread.read(cx);
                                    if !t.in_background() && !t.is_busy() {
                                        report(&thread, "end_turn", save.as_ref(), started, cx);
                                    }
                                });
                            })
                            .detach();
                        }
                    }
                    ThreadEvent::Changed
                    | ThreadEvent::Forked(_)
                    | ThreadEvent::Rewound { .. }
                    | ThreadEvent::RewindChanged { .. }
                    | ThreadEvent::LoginRequested
                    | ThreadEvent::ShellChanges
                    | ThreadEvent::CheckOffer { .. }
                    | ThreadEvent::Remote(_) => {}
                }
            }
        })
        .detach();

        let thread2 = thread.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(300)).await;
            println!("\n[probe] timed out");
            #[allow(clippy::diverging_sub_expression)]
            let _ = cx.update(|cx| finish(&thread2, 4, cx));
        })
        .detach();
    });
}
