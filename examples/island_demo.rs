//! Arakun's Dynamic Island (`src/island.rs`) with made-up agents: cycles
//! through its states every 4 s; hover it to list the agents.
//!
//!   cargo run --example island_demo [-- sleeping|working|speaking|listening|alert|list|alerts]
//!
//! A state given on the command line holds still; each alert shown is the next
//! of a few samples (a long reply, a long failure with a path, a short notice,
//! a long question). `alerts` plays them one after another, each put away
//! ("Later") after a few seconds, so the drip and the melt can be filmed with
//! ARAKUN_ISLAND_PASSTHROUGH=1. ISLAND_DEMO_SCREEN=n opens it on the nth screen
//! (Linux). ARAKUN_ISLAND_EDGE=notch or =pill draws it around a camera cutout
//! the screen doesn't have (see `src/island/edge.rs`).
//! Once clicked the island takes keys: 1-5 jump to a state, 6 pins the list,
//! space goes to the next, p pauses.

// Where it can't run, island.rs allows dead code itself; a second allow trips clippy.
#[cfg_attr(any(target_os = "linux", target_os = "macos"), allow(dead_code))]
#[path = "../src/island.rs"]
mod island;
#[allow(dead_code)]
#[path = "../src/mascot.rs"]
mod mascot;
#[allow(dead_code)]
#[path = "../src/theme.rs"]
mod theme;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn main() {
    demo::main();
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn main() {
    eprintln!("island_demo needs macOS, Windows, or a Wayland compositor with layer-shell");
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod demo {
    use std::time::{Duration, Instant};

    use gpui::{
        App, AppContext as _, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, MouseButton,
        ParentElement as _, Render, Styled as _, Subscription, Window, div, prelude::*,
    };

    use crate::island::{Alert, AlertKind, Feed, Island, IslandEvent, Phase, Row, Status};

    const CYCLE: Duration = Duration::from_secs(4);
    /// In the `alerts` tour, how long a card with buttons hangs before "Later".
    const READ_FOR: Duration = Duration::from_millis(3500);
    const PHASES: [Phase; 5] = [
        Phase::Sleeping,
        Phase::Working,
        Phase::Speaking,
        Phase::Listening,
        Phase::Alert,
    ];

    fn feed() -> Feed {
        let row = |status, model: &str, doing: &str, elapsed: &str, cost: &str| Row {
            key: model.to_string().into(),
            status,
            model: model.to_string().into(),
            doing: doing.to_string().into(),
            elapsed: elapsed.to_string().into(),
            cost: cost.to_string().into(),
            updated: 0,
        };
        Feed {
            rows: vec![
                row(
                    Status::Working,
                    "GLM-5.3",
                    "bench qkv m32 on the H100, round 4 of 6 with whole_compare",
                    "12:41",
                    "$0.84",
                ),
                row(
                    Status::Waiting,
                    "Opus 5.5",
                    "Apply the patch to src/shell/rail.rs and run the tests?",
                    "3:07",
                    "$1.92",
                ),
                row(
                    Status::Error,
                    "Kimi K3",
                    "cargo test: 2 failures in usage_history::rolls_over_midnight",
                    "0:58",
                    "$0.11",
                ),
            ],
            today: Some("$2.87".into()),
            viewing: None,
        }
    }

    fn samples() -> [Alert; 4] {
        let alert = |key: &str, kind, title: &str, subtitle: &str| Alert {
            key: key.to_string().into(),
            kind,
            title: title.to_string().into(),
            subtitle: subtitle.to_string().into(),
            req: Default::default(),
            image: Default::default(),
            brand: Default::default(),
        };
        [
            alert(
                "Opus 5.5",
                AlertKind::Finished,
                "Opus 5.5 finished",
                "Done: moved the deploy, CI, limit and Cloudflare notices to the island, fixed the reconnect that \
                 lost alerts and added tests for the queue of long notices. The fallback without Wayland is left \
                 for you to try.",
            ),
            alert(
                "open:runs",
                AlertKind::Failure,
                "Deploy of doop-selfhosted-containers-staging failed on Cloudflare",
                "wrangler deploy exited with code 1: the image \
                 registry.cloudflare.com/acme/doop:2026-11-05-1742/layers/sha256-9f2c7e1b44a0d3 weighs 2.4 GB and the \
                 plan allows 2 GB.",
            ),
            alert(
                "",
                AlertKind::Success,
                "Deploy ready",
                "doop · wrangler deploy in 41 s",
            ),
            alert(
                "Sonnet 5.5",
                AlertKind::Input,
                "Sonnet 5.5 needs your decision in \"Agent swarm with Cloudflare\"",
                "Delete the 14 old feature/* branches already merged into main, or only the ones with no commits \
                 for more than 30 days?",
            ),
        ]
    }

    struct Demo {
        island: Entity<Island>,
        phase: Phase,
        changed: Instant,
        held: bool,
        /// The next sample alert to show.
        sample: usize,
        /// Playing the samples one after another.
        tour: bool,
        focus: FocusHandle,
        _events: Subscription,
    }

    impl Demo {
        fn new(phase: Option<Phase>, pinned: bool, tour: bool, cx: &mut Context<Self>) -> Self {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    let alive = this.update(cx, |demo, cx| {
                        if demo.tour {
                            demo.step_tour(cx);
                        } else if !demo.held && demo.changed.elapsed() >= CYCLE {
                            demo.next(cx);
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
            let island = cx.new(|cx| {
                let mut island = Island::new(cx);
                island.set_feed(feed(), cx);
                island.pin(pinned, cx);
                island
            });
            let events = cx.subscribe(&island, |demo, _, ev, cx| {
                if let IslandEvent::Dismissed = ev
                    && demo.phase == Phase::Alert
                {
                    demo.go(
                        if demo.tour {
                            Phase::Working
                        } else {
                            Phase::Sleeping
                        },
                        cx,
                    );
                }
            });
            let start = if tour {
                Phase::Working
            } else {
                phase.unwrap_or(Phase::Sleeping)
            };
            let mut demo = Demo {
                island,
                phase: start,
                changed: Instant::now(),
                held: phase.is_some() || pinned || tour,
                sample: 0,
                tour,
                focus: cx.focus_handle(),
                _events: events,
            };
            demo.go(start, cx);
            demo
        }

        fn go(&mut self, phase: Phase, cx: &mut Context<Self>) {
            self.phase = phase;
            self.changed = Instant::now();
            let alert = (phase == Phase::Alert).then(|| {
                let all = samples();
                self.sample += 1;
                all[(self.sample - 1) % all.len()].clone()
            });
            self.island.update(cx, |island, cx| {
                if let Some(alert) = alert {
                    island.alert(alert, cx);
                }
                island.hold(Some(phase), cx);
            });
        }

        /// The tour: a sample, "Later" after a while (a notice without buttons
        /// times out by itself), a pause, the next.
        fn step_tour(&mut self, cx: &mut Context<Self>) {
            let since = self.changed.elapsed();
            if self.phase != Phase::Alert {
                if since >= Duration::from_millis(1500) {
                    self.go(Phase::Alert, cx);
                }
            } else if since >= READ_FOR {
                self.island.update(cx, |island, cx| island.later(cx));
            }
        }

        fn next(&mut self, cx: &mut Context<Self>) {
            let i = PHASES.iter().position(|&p| p == self.phase).unwrap_or(0);
            self.go(PHASES[(i + 1) % PHASES.len()], cx);
        }

        fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
            match e.keystroke.key.as_str() {
                k @ ("1" | "2" | "3" | "4" | "5") => {
                    self.held = true;
                    self.go(PHASES[k.parse::<usize>().unwrap_or(1) - 1], cx);
                }
                "6" => self.island.update(cx, |island, cx| {
                    let on = !island.pinned();
                    island.pin(on, cx);
                }),
                "space" => self.next(cx),
                "p" => self.held = !self.held,
                _ => {}
            }
        }
    }

    impl Render for Demo {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.key(e, cx)))
                // A click shows the next state; the list waits until the pointer
                // comes back. The card's buttons and the rows stop it.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        if this.phase != Phase::Alert {
                            this.next(cx);
                        }
                    }),
                )
                .child(self.island.clone())
        }
    }

    pub fn main() {
        let arg = std::env::args().nth(1).unwrap_or_default();
        let phase = Phase::from_name(&arg).filter(|p| *p != Phase::Idle);
        let pinned = arg == "list";
        let tour = arg == "alerts";
        let screen: Option<usize> = std::env::var("ISLAND_DEMO_SCREEN")
            .ok()
            .and_then(|s| s.parse().ok());
        gpui_platform::application().run(move |cx: &mut App| {
            cx.spawn(async move |cx| {
                // Wayland names its screens a moment after start.
                for _ in 0..80 {
                    if screen.is_none() || cx.update(|cx| !cx.displays().is_empty()) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(25))
                        .await;
                }
                cx.update(|cx| {
                    let mut options = crate::island::window_options(true);
                    options.display_id = screen.and_then(|n| cx.displays().get(n).map(|d| d.id()));
                    #[cfg(windows)]
                    if let Some(at) = crate::island::top_center(cx, options.display_id) {
                        options.window_bounds = Some(gpui::WindowBounds::Windowed(at));
                    }
                    let opened = cx.open_window(options, |window, cx| {
                        crate::island::adopt(window);
                        cx.new(|cx| Demo::new(phase, pinned, tour, cx))
                    });
                    if let Err(e) = opened {
                        eprintln!("island_demo: {e}");
                        cx.quit();
                    }
                });
            })
            .detach();
        });
    }
}
