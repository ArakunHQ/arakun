//! Headless check of the native Droid backend (`stream-jsonrpc`): runs a script
//! of steps against one session and prints the transcript, usage and settings.
//!
//!   cargo run --example native_demo -- <cwd> [steps and options]
//!
//! Steps run in order, each once the previous one settled:
//!   --prompt <text>         send a prompt and wait for the turn to end
//!   --set <setting>=<id>    change model, effort, autonomy or mode (auto|spec)
//!   --compact               compact the conversation
//!   --rename <title>        rename the session (droid.rename_session)
//!   --fork                  fork the session and print the new session id
//! Options:
//!   --model/--effort/--autonomy/--mode <id>   settings to start the session with
//!   --deny                  deny permission requests (default: allow / approve plans)
//!   --resume <session-id>   load an existing Droid session (history comes from Droid)
//!   --save <path>           write the transcript when the script ends
//!   --restore <path>        reopen a saved transcript and resume its session
//!   --list                  print the setting choices once connected
//!   --catalog               print Droid's defaults, model count and its sessions
//!                           for <cwd> without starting a session, then exit
//! ARAKUN_ACP_TRACE=1 dumps the raw JSON-RPC traffic on stderr.

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
#[allow(dead_code)]
#[path = "../src/ui.rs"]
mod ui;
#[path = "../src/zoom.rs"]
#[allow(dead_code)]
mod zoom;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agent::{
    AgentKind, Backend, Entry, SessionMeta, Setting, Thread, ThreadEvent, ThreadStatus, ToolStatus,
};
use gpui::{App, Entity};

enum Step {
    Prompt(String),
    Set(Setting, String),
    Compact,
    Rename(String),
    Fork,
}

struct Args {
    cwd: PathBuf,
    steps: VecDeque<Step>,
    initial: Vec<(Setting, String)>,
    allow: bool,
    resume: Option<String>,
    save: Option<PathBuf>,
    restore: Option<PathBuf>,
    list: bool,
    catalog: bool,
}

fn setting_of(name: &str) -> Option<Setting> {
    Some(match name {
        "model" => Setting::Model,
        "effort" | "reasoning" => Setting::Effort,
        "autonomy" => Setting::Autonomy,
        "mode" => Setting::Mode,
        _ => return None,
    })
}

fn parse_args() -> Args {
    let usage = || -> ! {
        eprintln!(
            "usage: native_demo <cwd> [--prompt t] [--set k=v] [--compact] [--model m] [--effort e] [--autonomy a] [--mode auto|spec] [--deny] [--resume id] [--save p] [--restore p] [--list]"
        );
        std::process::exit(2)
    };
    let mut it = std::env::args().skip(1);
    let cwd = it.next().map(PathBuf::from).unwrap_or_else(|| usage());
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let mut a = Args {
        cwd,
        steps: VecDeque::new(),
        initial: Vec::new(),
        allow: true,
        resume: None,
        save: None,
        restore: None,
        list: false,
        catalog: false,
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--prompt" => a.steps.push_back(Step::Prompt(value())),
            "--set" => {
                let v = value();
                let (k, v) = v.split_once('=').unwrap_or_else(|| usage());
                let s = setting_of(k).unwrap_or_else(|| usage());
                a.steps.push_back(Step::Set(s, v.to_string()));
            }
            "--compact" => a.steps.push_back(Step::Compact),
            "--rename" => a.steps.push_back(Step::Rename(value())),
            "--fork" => a.steps.push_back(Step::Fork),
            "--catalog" => a.catalog = true,
            "--model" | "--effort" | "--autonomy" | "--mode" => {
                let s = setting_of(&flag[2..]).unwrap();
                a.initial.push((s, value()));
            }
            "--deny" => a.allow = false,
            "--resume" => a.resume = Some(value()),
            "--save" => a.save = Some(PathBuf::from(value())),
            "--restore" => a.restore = Some(PathBuf::from(value())),
            "--list" => a.list = true,
            _ => usage(),
        }
    }
    a
}

fn print_entries(entries: &[Entry], printed: &mut Vec<String>) {
    if entries.len() < printed.len() {
        printed.clear();
    }
    for (i, e) in entries.iter().enumerate() {
        let line = match e {
            Entry::User { text, .. } => format!("[user] {text}"),
            Entry::Agent { text } => format!("[agent] {text}"),
            Entry::Thought { text } => format!("[thought] {text}"),
            Entry::Tool(t) => format!(
                "[tool {} {:?}] {}{}{}",
                t.kind,
                t.status,
                t.title,
                if t.added + t.removed > 0 {
                    format!(" +{} -{}", t.added, t.removed)
                } else {
                    String::new()
                },
                match (&t.raw_output, t.status) {
                    (Some(o), ToolStatus::Completed | ToolStatus::Failed) => {
                        format!(
                            " => {}",
                            o.lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(80)
                                .collect::<String>()
                        )
                    }
                    _ => String::new(),
                }
            ),
            Entry::Plan { entries } => format!(
                "[plan] {}",
                entries
                    .iter()
                    .map(|p| format!("{}:{}", p.status, p.content))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            Entry::Notice { text, error, .. } => {
                format!("[notice{}] {text}", if *error { " error" } else { "" })
            }
            Entry::Compacted { before, after, .. } => {
                format!("[compacted {before:?}% -> {after:?}%]")
            }
        };
        // Streaming text is printed once it stops changing (on the next event or the end).
        if printed.get(i) != Some(&line) {
            if i < printed.len() {
                printed[i] = line.clone();
            } else {
                printed.push(line.clone());
            }
            let streaming =
                matches!(e, Entry::Agent { .. } | Entry::Thought { .. }) && i + 1 == entries.len();
            if !streaming {
                println!("{line}");
            }
        }
    }
}

fn settings_line(t: &Thread) -> String {
    Setting::ALL
        .iter()
        .map(|s| {
            format!(
                "{}={}",
                s.label().to_lowercase(),
                t.setting(*s).map(|(c, _)| c).unwrap_or_else(|| "?".into())
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn finish(thread: &Entity<Thread>, code: i32, cx: &mut App) -> ! {
    thread.update(cx, |t, _| t.shutdown());
    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(code)
}

struct Run {
    args: Args,
    printed: Vec<String>,
    started: bool,
    compacting: bool,
    clock: Instant,
}

fn next_step(run: &Rc<RefCell<Run>>, thread: &Entity<Thread>, cx: &mut App) {
    loop {
        let step = run.borrow_mut().args.steps.pop_front();
        let Some(step) = step else {
            let t = thread.read(cx);
            let mut r = run.borrow_mut();
            print_entries(t.entries(), &mut r.printed);
            if let Some(last) = r.printed.last() {
                println!("{last}");
            }
            println!("[settings] {}", settings_line(t));
            println!("[usage] {}", t.usage().summary().unwrap_or_default());
            println!(
                "[meta] {}",
                serde_json::to_string(&t.meta()).unwrap_or_default()
            );
            if let Some(path) = &r.args.save {
                match t.save_transcript(path, cx) {
                    Ok(()) => println!("[saved] {}", path.display()),
                    Err(e) => println!("[save failed] {e:#}"),
                }
            }
            println!("[done] {:.1}s", r.clock.elapsed().as_secs_f32());
            drop(r);
            finish(thread, 0, cx);
        };
        match step {
            Step::Prompt(text) => {
                println!("[step] prompt");
                thread.update(cx, |t, cx| t.send(text, cx));
                return;
            }
            Step::Set(s, v) => {
                println!("[step] set {}={v}", s.label());
                thread.update(cx, |t, cx| t.set_setting(s, &v, cx));
            }
            Step::Compact => {
                println!("[step] compact");
                run.borrow_mut().compacting = true;
                thread.update(cx, |t, cx| t.compact(None, cx));
                return;
            }
            Step::Rename(title) => {
                println!("[step] rename {title:?}");
                thread.update(cx, |t, cx| t.rename(&title, cx));
                // The rename has no reply event; give Droid a moment to store it.
                let (run, thread) = (run.clone(), thread.clone());
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1500))
                        .await;
                    cx.update(|cx| next_step(&run, &thread, cx));
                })
                .detach();
                return;
            }
            Step::Fork => {
                println!("[step] fork");
                thread.update(cx, |t, cx| t.fork(cx));
                return;
            }
        }
    }
}

fn print_catalog(cwd: &std::path::Path) {
    let c = agent::catalog::refresh();
    let d = &c.defaults;
    println!(
        "[defaults] model={:?} effort={:?} autonomy={:?} mode={:?}",
        d.model_id, d.reasoning_effort, d.autonomy_level, d.interaction_mode
    );
    println!(
        "[models] {}",
        c.models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("[slug] {}", agent::catalog::session_slug(cwd));
    for s in agent::catalog::droid_sessions(cwd) {
        println!(
            "[droid session] {} updated={} {:?}",
            s.id, s.updated, s.title
        );
    }
}

fn main() {
    let args = parse_args();
    if args.catalog {
        print_catalog(&args.cwd);
        return;
    }
    gpui_platform::headless().run(move |cx: &mut App| {
        let thread = match &args.restore {
            Some(path) => Thread::restore(path, cx).unwrap_or_else(|e| {
                eprintln!("restore failed: {e:#}");
                std::process::exit(1)
            }),
            None => {
                let resume = args.resume.clone().map(|id| SessionMeta {
                    agent: AgentKind::Droid,
                    session_id: Some(id),
                    title: String::new(),
                    cwd: args.cwd.clone(),
                    created: 0,
                    updated: 0,
                    pr: None,
                });
                Thread::start(AgentKind::Droid, args.cwd.clone(), resume, cx)
            }
        };
        if thread.read(cx).backend() != Backend::Native {
            eprintln!("native_demo: ARAKUN_DROID_ACP is set, so Droid runs over ACP");
            std::process::exit(2);
        }
        thread.update(cx, |t, cx| {
            for (s, v) in &args.initial {
                t.set_setting(*s, v, cx);
            }
        });
        let run = Rc::new(RefCell::new(Run {
            args,
            printed: Vec::new(),
            started: false,
            compacting: false,
            clock: Instant::now(),
        }));
        print_entries(thread.read(cx).entries(), &mut run.borrow_mut().printed);

        cx.subscribe(&thread, {
            let run = run.clone();
            move |thread, ev, cx| match ev {
                ThreadEvent::EntriesChanged { .. } | ThreadEvent::Reset => {
                    let t = thread.read(cx);
                    print_entries(t.entries(), &mut run.borrow_mut().printed);
                }
                ThreadEvent::StatusChanged => {
                    let status = thread.read(cx).status();
                    match status {
                        ThreadStatus::Idle if !run.borrow().started => {
                            run.borrow_mut().started = true;
                            let t = thread.read(cx);
                            println!(
                                "[session] id={} title={:?} models={}",
                                t.session_id().unwrap_or("-"),
                                t.title(),
                                t.model_catalog().len()
                            );
                            println!("[settings] {}", settings_line(t));
                            if run.borrow().args.list {
                                for s in Setting::ALL {
                                    if let Some((cur, opts)) = t.setting(s) {
                                        println!(
                                            "[choices {}] current={cur}: {}",
                                            s.label(),
                                            opts.iter()
                                                .map(|o| match (&o.detail, &o.disabled) {
                                                    (_, Some(why)) => format!("{} (disabled: {why})", o.id),
                                                    (Some(d), None) => format!("{} {d}", o.id),
                                                    _ => o.id.clone(),
                                                })
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        );
                                    }
                                }
                            }
                            next_step(&run, &thread, cx);
                        }
                        // A compaction settles without a Finished event.
                        ThreadStatus::Idle if run.borrow().compacting => {
                            run.borrow_mut().compacting = false;
                            let t = thread.read(cx);
                            print_entries(t.entries(), &mut run.borrow_mut().printed);
                            println!("[compacted] session={}", t.session_id().unwrap_or("-"));
                            next_step(&run, &thread, cx);
                        }
                        ThreadStatus::Error(e) => {
                            println!("[error] {e}");
                            finish(&thread, 1, cx);
                        }
                        _ => {}
                    }
                }
                ThreadEvent::NeedsPermission => {
                    let allow = run.borrow().args.allow;
                    let t = thread.read(cx);
                    if let Some(q) = t.pending_question() {
                        let answers: Vec<String> = q
                            .questions
                            .iter()
                            .map(|q| {
                                println!(
                                    "[ask] {}: {} options={:?} multi={}",
                                    q.topic, q.question, q.options, q.multi
                                );
                                q.options.first().cloned().unwrap_or_else(|| "yes".into())
                            })
                            .collect();
                        println!("[demo] answering {answers:?}");
                        thread.update(cx, |t, cx| t.answer_question(answers, cx));
                    } else if let Some(p) = t.pending_permission() {
                        println!(
                            "[permission] {} options: {}",
                            p.title,
                            p.options
                                .iter()
                                .map(|o| format!("{}={}({})", o.id, o.name, o.kind))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        if let Some(plan) = &p.plan {
                            println!("[spec plan] {} :: {}", plan.title, plan.plan.replace('\n', " / "));
                        }
                        println!("[demo] answering {}", if allow { "allow" } else { "deny" });
                        thread.update(cx, |t, cx| {
                            if allow {
                                t.allow_permission(cx)
                            } else {
                                t.reject_permission(cx)
                            }
                        });
                    }
                }
                ThreadEvent::TitleChanged => println!("[title] {}", thread.read(cx).title()),
                ThreadEvent::Forked(meta) => {
                    println!("[forked] {}", serde_json::to_string(meta).unwrap_or_default());
                    next_step(&run, &thread, cx);
                }
                ThreadEvent::Rewound { meta, prompt } => {
                    println!("[rewound] {} prompt={prompt:?}", serde_json::to_string(meta).unwrap_or_default());
                }
                ThreadEvent::RewindChanged { .. }
                | ThreadEvent::FailureReportDue
                | ThreadEvent::LoginRequested
                | ThreadEvent::SubagentsChanged { .. }
                | ThreadEvent::ShellChanges
                | ThreadEvent::CheckOffer { .. } => {}
                ThreadEvent::Finished { stop_reason } => {
                    let t = thread.read(cx);
                    let mut r = run.borrow_mut();
                    print_entries(t.entries(), &mut r.printed);
                    if let Some(last) = r.printed.last() {
                        println!("{last}");
                    }
                    println!(
                        "[finished] stop={stop_reason} usage: {} | in={} out={} cache_r={} cache_w={} credits={:?} ({:.1}s)",
                        t.usage().summary().unwrap_or_default(),
                        t.usage().input_tokens,
                        t.usage().output_tokens,
                        t.usage().cache_read_tokens,
                        t.usage().cache_write_tokens,
                        t.usage().credits,
                        r.clock.elapsed().as_secs_f32()
                    );
                    println!("[settings] {}", settings_line(t));
                    drop(r);
                    // Context stats arrive just after the turn; give them a moment.
                    let thread = thread.clone();
                    let run = run.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_millis(600)).await;
                        cx.update(|cx| {
                            println!("[context] {}", thread.read(cx).usage().summary().unwrap_or_default());
                            next_step(&run, &thread, cx)
                        });
                    })
                    .detach();
                }
                ThreadEvent::Changed => {}
            }
        })
        .detach();

        let thread2 = thread.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(600)).await;
            println!("[demo] timed out");
            #[allow(clippy::diverging_sub_expression)]
            let _ = cx.update(|cx| finish(&thread2, 4, cx));
        })
        .detach();
    });
}
