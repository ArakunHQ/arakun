//! `arakun island-host`: the Dynamic Island (`crate::island`) as a program of
//! its own, started with the session (Hyprland `exec-once`, a systemd user
//! unit) or by the app. It has only the island's window, so it paints at the
//! display's rate whatever the app's window costs, and stays up with the app
//! closed.
//!
//! It listens on [`proto::socket_path`]. The app connects ([`link`]) and feeds
//! it its sessions and alerts; `arakun island …` sends one request; the host
//! itself watches the headless agents' folders ([`watch`]). With no app, a
//! click on the island starts it.
//!
//! Everything that changes what the island shows goes through [`Host::apply`]
//! as a [`proto::ToIsland`], and what the user presses comes out of
//! [`Host::on_island`]. A voice brain is meant to live here as one more source:
//! it sends `Hold` (speaking, listening) and alerts through `apply`, and hears
//! the presses next to the app.

// The island needs a Wayland layer shell, macOS or Windows: elsewhere it builds
// but never runs.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    allow(dead_code, unused_imports)
)]

#[cfg(all(target_os = "linux", feature = "coderlabs"))]
pub mod call;
#[cfg(target_os = "linux")]
pub mod desktop;
#[cfg(windows)]
pub mod external;
#[cfg(any(unix, windows))]
pub mod link;
#[cfg(all(target_os = "linux", feature = "coderlabs"))]
pub mod meeting;
pub mod proto;
#[cfg(feature = "coderlabs")]
pub mod wake;
pub mod watch;
#[cfg(windows)]
mod windows;

/// `arakun island …`: one request, straight to the island.
pub fn cli(args: &[String]) -> i32 {
    #[cfg(windows)]
    if let Some(code) = external::cli(args) {
        return code;
    }
    let Some(msg) = proto::from_args(args) else {
        eprintln!("usage: {}", crate::control::ISLAND_USAGE);
        return 2;
    };
    send(msg)
}

#[cfg(feature = "coderlabs")]
/// `arakun voz [on|off]`: starts, ends or flips the island's voice call.
pub fn voz(args: &[String]) -> i32 {
    let Some(msg) = proto::voz_args(args) else {
        eprintln!("usage: {VOZ_USAGE}");
        return 2;
    };
    send(msg)
}

#[cfg(feature = "coderlabs")]
pub const VOZ_USAGE: &str = "arakun voz [on|off]";

#[cfg(feature = "coderlabs")]
/// `arakun reunion [on|off] [personas]`: records a meeting (mic and speakers).
pub fn reunion(args: &[String]) -> i32 {
    let Some(msg) = proto::reunion_args(args) else {
        eprintln!("usage: {REUNION_USAGE}");
        return 2;
    };
    send(msg)
}

#[cfg(feature = "coderlabs")]
pub const REUNION_USAGE: &str = "arakun reunion [on|off] [personas]";

fn send(msg: proto::ToIsland) -> i32 {
    #[cfg(any(unix, windows))]
    {
        match proto::request(&msg) {
            Ok(reply) if reply.ok => {
                if !reply.message.is_empty() {
                    println!("{}", reply.message);
                }
                0
            }
            Ok(reply) => fail(&reply.message),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                fail("the island is not running; start it with `arakun island-host`")
            }
            Err(e) => fail(&format!("could not reach the island: {e}")),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = msg;
        fail(NEEDS)
    }
}

/// Why there is no island here.
const NEEDS: &str = "the island needs macOS, or Linux on Wayland with layer-shell";

fn fail(message: &str) -> i32 {
    eprintln!("arakun: {message}");
    1
}

/// How much of a reply an alert card carries; the card wraps it in a few lines.
const BRIEF_CHARS: usize = 300;

/// A reply as an alert card tells it: its opening paragraphs as running text,
/// with markdown markers, links and code blocks off, cut at `BRIEF_CHARS`.
pub fn brief(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut len = 0;
    let mut fenced = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        // A rule or a table's separator says nothing.
        if fenced
            || line
                .chars()
                .all(|c| matches!(c, '-' | '*' | '_' | '=' | '|' | ':' | ' '))
        {
            continue;
        }
        let line = line.trim_start_matches(['#', '>', ' ']);
        let line = match line.split_once(' ') {
            Some((mark, rest))
                if matches!(mark, "-" | "*" | "+")
                    || mark
                        .strip_suffix(['.', ')'])
                        .is_some_and(|n| n.parse::<u32>().is_ok()) =>
            {
                rest
            }
            _ => line,
        };
        for word in unlink(line)
            .replace(['`', '*'], "")
            .replace('|', " ")
            .split_whitespace()
        {
            len += word.chars().count() + 1;
            words.push(word.to_string());
        }
        if len > BRIEF_CHARS {
            break;
        }
    }
    let text = words.join(" ");
    let Some((cut, _)) = text.char_indices().nth(BRIEF_CHARS) else {
        return text;
    };
    // At a word's end when one is near, else mid-word.
    let head = &text[..cut];
    let head = match head.rfind(' ') {
        Some(space) if cut - space < 40 => &head[..space],
        _ => head,
    };
    format!("{}…", head.trim_end_matches([' ', ',', ';', ':']))
}

/// `[text](url)` as `text`.
fn unlink(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after
            .find(']')
            .filter(|&close| after[close + 1..].starts_with('('))
            .and_then(|close| after[close..].find(')').map(|end| (close, close + end)));
        match link {
            Some((close, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn main() -> i32 {
    fail(NEEDS)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub use host::main;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod host {
    use std::collections::HashSet;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    #[cfg(feature = "coderlabs")]
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::{Duration, Instant};
    #[cfg(windows)]
    use uds_windows::{UnixListener, UnixStream};

    use futures::StreamExt as _;
    use futures::channel::{mpsc, oneshot};
    use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Subscription, Task};

    #[cfg(feature = "coderlabs")]
    use super::call::{self, Call};
    use super::proto::{self, Button, FromIsland, Reply, ToIsland};
    use super::watch::{self, Watch};
    #[cfg(feature = "coderlabs")]
    use crate::island::Levels;
    use crate::island::{self, Alert, AlertKind, Feed, Island, IslandEvent, Phase};
    use crate::ui::motion;

    /// How often the headless folders are read.
    const WATCH_EVERY: Duration = Duration::from_secs(1);
    /// A click starts the app at most this often, while it comes up.
    const LAUNCH_EVERY: Duration = Duration::from_secs(10);
    /// How long after `Quit` the host goes: time for the reply to leave first.
    const QUIT_AFTER: Duration = Duration::from_millis(100);
    /// A second click on the shape a button just shrank is not one on the resting island.
    const AFTER_BUTTON: Duration = Duration::from_secs(1);

    /// How often finished and silent outside sessions are looked for.
    #[cfg(windows)]
    const EXPIRE_EVERY: Duration = Duration::from_secs(30);

    static EXIT: AtomicI32 = AtomicI32::new(0);

    /// A card of an agent outside Arakun: the app leaving doesn't take it along.
    #[cfg(windows)]
    fn outside(key: &str) -> bool {
        key.starts_with(super::external::PREFIX)
    }

    /// Only Windows has agents outside Arakun on the island.
    #[cfg(not(windows))]
    fn outside(_: &str) -> bool {
        false
    }

    enum Msg {
        Hello {
            conn: u64,
            pid: u32,
            out: std::sync::mpsc::Sender<FromIsland>,
        },
        App {
            conn: u64,
            msg: ToIsland,
        },
        Gone {
            conn: u64,
        },
        Request {
            msg: ToIsland,
            done: oneshot::Sender<Reply>,
        },
        #[cfg(feature = "coderlabs")]
        Wake(super::wake::Wake),
        /// A desktop notification (`desktop.rs`), which only Linux passes on.
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        Notice(Alert),
    }

    struct AppLink {
        conn: u64,
        pid: u32,
        out: std::sync::mpsc::Sender<FromIsland>,
        feed: Feed,
    }

    struct Keep(#[allow(dead_code)] Entity<Host>);
    impl Global for Keep {}

    pub fn main() -> i32 {
        // Started from the Run key or Explorer, its own console window would sit by the island.
        #[cfg(windows)]
        crate::platform::detach_own_console();
        let path = proto::socket_path();
        if UnixStream::connect(&path).is_ok() {
            eprintln!("arakun: the island already runs on {}", path.display());
            return 0;
        }
        let listener = match bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("arakun: island socket {}: {e}", path.display());
                return 1;
            }
        };
        let (tx, rx) = mpsc::unbounded();
        #[cfg(feature = "coderlabs")]
        {
            let woke = tx.clone();
            super::wake::start(move |w| drop(woke.unbounded_send(Msg::Wake(w))));
        }
        #[cfg(target_os = "linux")]
        {
            let noticed = tx.clone();
            super::desktop::start(move |a| drop(noticed.unbounded_send(Msg::Notice(a))));
        }
        let accepted = std::thread::Builder::new()
            .name("arakun-island".into())
            .spawn(move || {
                let mut conns = 1..;
                crate::accept::serve(
                    listener.incoming(),
                    CONNECTIONS,
                    "arakun-island-conn",
                    || false,
                    |stream| {
                        let conn = conns.next().unwrap_or_default();
                        let tx = tx.clone();
                        move || serve(conn, stream, tx)
                    },
                );
            });
        if let Err(e) = accepted {
            eprintln!("arakun: island socket: {e}");
            return 1;
        }
        gpui::Application::with_platform(crate::ui_zoom::platform())
            .with_assets(crate::assets::Assets)
            .run(move |cx: &mut App| {
                crate::assets::load_fonts(cx);
                crate::signals::quit_on_signals(cx);
                crate::ui::motion::follow_system(cx);
                island::hide_from_dock();
                if !island::supported(cx) {
                    eprintln!("arakun: {}", super::NEEDS);
                    EXIT.store(1, Ordering::Relaxed);
                    cx.quit();
                    return;
                }
                // Wayland names its screens a moment after start: wait for them so the
                // island can pick the landscape one, then open it.
                cx.spawn(async move |cx| {
                    for _ in 0..80 {
                        if cx.update(|cx| !cx.displays().is_empty()) {
                            break;
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(25))
                            .await;
                    }
                    cx.update(|cx| open(cx, rx));
                })
                .detach();
            });
        let _ = std::fs::remove_file(&path);
        #[cfg(feature = "coderlabs")]
        super::call::kill_all();
        EXIT.load(Ordering::Relaxed)
    }

    /// The island's window, and the host that feeds it. On Linux it goes on
    /// the landscape screen, on macOS `island::adopt` places it, and on Windows
    /// it goes at the top center of the main display.
    fn open(cx: &mut App, rx: mpsc::UnboundedReceiver<Msg>) {
        let mut options = island::window_options(false);
        #[cfg(not(windows))]
        {
            options.display_id = island::landscape_display(cx);
        }
        #[cfg(windows)]
        super::windows::place(cx, &mut options);
        for d in cx.displays() {
            let b = d.bounds();
            eprintln!(
                "arakun: screen {:?} {}x{} at {},{}",
                d.id(),
                b.size.width.as_f32(),
                b.size.height.as_f32(),
                b.origin.x.as_f32(),
                b.origin.y.as_f32()
            );
        }
        eprintln!("arakun: island on {:?}", options.display_id);
        let opened = cx
            .open_window(options, |window, cx| {
                island::adopt(window);
                cx.new(Island::new)
            })
            .and_then(|handle| handle.update(cx, |_, _, cx| cx.entity()));
        let view = match opened {
            Ok(view) => view,
            Err(e) => {
                eprintln!("arakun: no island: {e}");
                EXIT.store(1, Ordering::Relaxed);
                cx.quit();
                return;
            }
        };
        let host = cx.new(|cx| Host::new(view, rx, cx));
        cx.set_global(Keep(host));
        // The compositor took the layer away (its output went): start over.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                EXIT.store(1, Ordering::Relaxed);
                cx.quit();
            }
        })
        .detach();
    }

    /// Every Arakun and every agent hook outside it connects; none needs long.
    const CONNECTIONS: crate::accept::Limits = crate::accept::Limits {
        total: 256,
        per_peer: 256,
    };

    fn bind(path: &Path) -> std::io::Result<UnixListener> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = match UnixListener::bind(path) {
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Left behind by a crash: nothing answered on it.
                std::fs::remove_file(path)?;
                UnixListener::bind(path)?
            }
            other => other?,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(listener)
    }

    /// One connection: the app's, kept, or a request, answered.
    fn serve(conn: u64, stream: UnixStream, tx: mpsc::UnboundedSender<Msg>) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut lines = BufReader::new((&stream).take(1 << 22)).lines();
        let first = match lines.next() {
            Some(Ok(line)) => proto::decode::<ToIsland>(&line),
            _ => return,
        };
        let mut out = &stream;
        match first {
            Ok(ToIsland::Hello { pid }) => {
                let _ = stream.set_read_timeout(None);
                let (events, presses) = std::sync::mpsc::channel::<FromIsland>();
                let Ok(writer) = stream.try_clone() else {
                    return;
                };
                std::thread::spawn(move || {
                    let mut writer = writer;
                    for ev in presses {
                        if writer.write_all(proto::encode(&ev).as_bytes()).is_err() {
                            break;
                        }
                    }
                });
                if tx
                    .unbounded_send(Msg::Hello {
                        conn,
                        pid,
                        out: events,
                    })
                    .is_err()
                {
                    return;
                }
                for line in lines {
                    let Ok(line) = line else { break };
                    match proto::decode(&line) {
                        Ok(msg) => {
                            let _ = tx.unbounded_send(Msg::App { conn, msg });
                        }
                        Err(e) => eprintln!("arakun: island: app {pid}: {e}"),
                    }
                }
                let _ = tx.unbounded_send(Msg::Gone { conn });
            }
            Ok(msg) => {
                let (done, wait) = oneshot::channel();
                let reply = if tx.unbounded_send(Msg::Request { msg, done }).is_err() {
                    Reply::err("the island is shutting down")
                } else {
                    futures::executor::block_on(wait)
                        .unwrap_or_else(|_| Reply::err("the island did not answer"))
                };
                let _ = out.write_all(proto::encode(&reply).as_bytes());
            }
            Err(e) => {
                let _ = out.write_all(proto::encode(&Reply::err(e)).as_bytes());
            }
        }
    }

    pub(super) struct Host {
        island: Entity<Island>,
        app: Option<AppLink>,
        watch: Watch,
        headless: watch::Seen,
        /// Agents outside Arakun, told by their hooks.
        #[cfg(windows)]
        external: super::external::Sessions,
        launched: Option<Instant>,
        /// When a card's button or a row was last pressed.
        button_at: Option<Instant>,
        /// The voice call going on, and its number: what a finished one says is ignored.
        #[cfg(feature = "coderlabs")]
        call: Option<(u64, Call)>,
        /// The call opened on "Oye Ara…", not yet confirmed.
        #[cfg(feature = "coderlabs")]
        tentative: bool,
        #[cfg(feature = "coderlabs")]
        calls: u64,
        #[cfg(feature = "coderlabs")]
        levels: Arc<Levels>,
        /// The meeting being recorded, and its levels for the green wave.
        #[cfg(feature = "coderlabs")]
        meeting: Option<super::meeting::Recording>,
        #[cfg(feature = "coderlabs")]
        meeting_levels: Arc<Levels>,
        #[cfg(feature = "coderlabs")]
        heard: mpsc::UnboundedSender<(u64, call::Out)>,
        _events: Subscription,
        _tasks: Vec<Task<()>>,
    }

    impl Host {
        fn new(
            island: Entity<Island>,
            mut rx: mpsc::UnboundedReceiver<Msg>,
            cx: &mut Context<Self>,
        ) -> Self {
            let inbox = cx.spawn(async move |this, cx| {
                while let Some(msg) = rx.next().await {
                    if this.update(cx, |host, cx| host.handle(msg, cx)).is_err() {
                        break;
                    }
                }
            });
            let headless = cx.spawn(async move |this, cx| {
                loop {
                    let scan = cx
                        .background_spawn(crate::guard::task(async { Some(watch::scan()) }))
                        .await;
                    let Some(scan) = scan else {
                        // A scan that broke says nothing about the agents; the next may work.
                        cx.background_executor().timer(WATCH_EVERY).await;
                        continue;
                    };
                    let Ok(mut seen) = this.update(cx, |host, _| host.watch.update(&scan)) else {
                        break;
                    };
                    let seen = cx
                        .background_spawn(crate::guard::task_or_default(async move {
                            watch::read_replies(&mut seen.alerts);
                            seen
                        }))
                        .await;
                    if this.update(cx, |host, cx| host.headless(seen, cx)).is_err() {
                        break;
                    }
                    cx.background_executor().timer(WATCH_EVERY).await;
                }
            });
            // Outside agents may go quiet for good: their rows still have to leave.
            #[cfg(windows)]
            let expiry = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(EXPIRE_EVERY).await;
                    let now = crate::model::unix_now();
                    let alive = this.update(cx, |host, cx| {
                        if host.external.expire(now) {
                            host.push(cx);
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            });
            let events = cx.subscribe(&island, |host, _, ev, cx| host.on_island(ev, cx));
            #[cfg(feature = "coderlabs")]
            let (heard, mut calls) = mpsc::unbounded::<(u64, call::Out)>();
            #[cfg(feature = "coderlabs")]
            let voice = cx.spawn(async move |this, cx| {
                while let Some((n, out)) = calls.next().await {
                    if this
                        .update(cx, |host, cx| host.on_call(n, out, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Host {
                island,
                app: None,
                watch: Watch::default(),
                headless: watch::Seen::default(),
                #[cfg(windows)]
                external: super::external::Sessions::default(),
                launched: None,
                button_at: None,
                #[cfg(feature = "coderlabs")]
                call: None,
                #[cfg(feature = "coderlabs")]
                tentative: false,
                #[cfg(feature = "coderlabs")]
                calls: 0,
                #[cfg(feature = "coderlabs")]
                levels: Arc::new(Levels::default()),
                #[cfg(feature = "coderlabs")]
                meeting: None,
                #[cfg(feature = "coderlabs")]
                meeting_levels: Arc::new(Levels::default()),
                #[cfg(feature = "coderlabs")]
                heard,
                _events: events,
                _tasks: vec![
                    inbox,
                    headless,
                    #[cfg(feature = "coderlabs")]
                    voice,
                    #[cfg(windows)]
                    expiry,
                ],
            }
        }

        fn handle(&mut self, msg: Msg, cx: &mut Context<Self>) {
            match msg {
                Msg::Hello { conn, pid, out } => {
                    eprintln!("arakun: island: app {pid} connected");
                    self.app = Some(AppLink {
                        conn,
                        pid,
                        out,
                        feed: Feed::default(),
                    });
                    self.push(cx);
                }
                Msg::App { conn, msg } if self.app.as_ref().is_some_and(|a| a.conn == conn) => {
                    match msg {
                        ToIsland::Feed { feed } => {
                            if let Some(app) = &mut self.app {
                                app.feed = feed;
                            }
                            self.push(cx);
                        }
                        msg => {
                            self.apply(msg, cx);
                        }
                    }
                }
                Msg::App { .. } => {}
                Msg::Gone { conn } if self.app.as_ref().is_some_and(|a| a.conn == conn) => {
                    let pid = self.app.take().map(|a| a.pid).unwrap_or_default();
                    eprintln!("arakun: island: app {pid} left");
                    // Its sessions go with it; the headless ones, notices and the island's own stay.
                    let headless: HashSet<SharedString> =
                        self.headless.rows.iter().map(|r| r.key.clone()).collect();
                    let busy = self.headless.rows.iter().any(|r| r.status.active());
                    self.island.update(cx, |island, cx| {
                        island.retain_alerts(
                            |a| {
                                !a.kind.session()
                                    || a.key.is_empty()
                                    || a.kind == AlertKind::Confirm
                                    || headless.contains(&a.key)
                                    || outside(&a.key)
                            },
                            cx,
                        );
                        if !busy {
                            island.forget_busy(cx);
                        }
                    });
                    self.push(cx);
                }
                Msg::Gone { .. } => {}
                Msg::Request { msg, done } => {
                    let _ = done.send(self.apply(msg, cx));
                }
                #[cfg(feature = "coderlabs")]
                Msg::Wake(w) => self.wake(w, cx),
                Msg::Notice(alert) => {
                    self.apply(ToIsland::Alert { alert }, cx);
                }
            }
        }

        /// What any source asks of the island.
        pub(super) fn apply(&mut self, msg: ToIsland, cx: &mut Context<Self>) -> Reply {
            if msg == ToIsland::Quit {
                eprintln!("arakun: island: asked to quit");
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(QUIT_AFTER).await;
                    // macOS ends the process inside `quit`, before `main` gets to it.
                    let _ = std::fs::remove_file(proto::socket_path());
                    cx.update(|cx| cx.quit());
                })
                .detach();
                return Reply::ok("");
            }
            #[cfg(windows)]
            let msg = match msg {
                ToIsland::Session { update } => return self.session(update, cx),
                msg => msg,
            };
            #[cfg(feature = "coderlabs")]
            match &msg {
                ToIsland::Voice { on } => return self.voice(*on, cx),
                ToIsland::Meeting { on, people } => return self.meeting(*on, *people, cx),
                ToIsland::Wake { state } => {
                    use super::wake::Wake;
                    let w = match state.as_str() {
                        "maybe" => Wake::Maybe,
                        "yes" => Wake::Yes(None),
                        _ => Wake::No,
                    };
                    self.wake(w, cx);
                    return Reply::ok(format!("{w:?}"));
                }
                ToIsland::Alert { alert } => self.tell_call(alert),
                _ => {}
            }
            self.island.update(cx, |island, cx| match msg {
                ToIsland::Hello { .. } | ToIsland::Feed { .. } => {
                    Reply::err("only a connected app sends that")
                }
                ToIsland::Alert { alert } => {
                    island.alert(alert, cx);
                    Reply::ok("")
                }
                ToIsland::Hold { phase } => {
                    island.pin(false, cx);
                    island.hold(phase, cx);
                    Reply::ok("")
                }
                ToIsland::Pin { on } => {
                    let on = on.unwrap_or(!island.pinned());
                    island.pin(on, cx);
                    Reply::ok("")
                }
                ToIsland::Press { button } => {
                    match button {
                        Button::Ver => island.see(cx),
                        Button::Luego => island.later(cx),
                        Button::Aprobar => island.answer(true, cx),
                        Button::Rechazar => island.answer(false, cx),
                    }
                    Reply::ok("")
                }
                // Handled above, before the island is asked.
                ToIsland::Quit => Reply::ok(""),
                ToIsland::State => Reply::ok(format!("{:?}", island.phase()).to_lowercase()),
                ToIsland::Fps => Reply::ok(island.fps_report()),
                #[cfg(windows)]
                ToIsland::Session { .. } => Reply::ok(""),
                #[cfg(feature = "coderlabs")]
                ToIsland::Voice { .. } | ToIsland::Meeting { .. } | ToIsland::Wake { .. } => {
                    Reply::ok("")
                }
            })
        }

        fn headless(&mut self, mut seen: watch::Seen, cx: &mut Context<Self>) {
            let changed = seen.rows != self.headless.rows || seen.today != self.headless.today;
            let pending = seen.pending.clone();
            let alerts = std::mem::take(&mut seen.alerts);
            let gone = pending != self.headless.pending;
            self.headless = seen;
            // An agent the voice launched comes back to say what it found,
            // calling again if the call already ended.
            #[cfg(feature = "coderlabs")]
            for alert in &alerts {
                if matches!(alert.kind, AlertKind::Finished | AlertKind::Error)
                    && call::take_launched(&alert.key)
                {
                    match &self.call {
                        Some((_, c)) => c.report(alert),
                        None => self.open_call(Some(alert.clone()), cx),
                    }
                } else {
                    self.tell_call(alert);
                }
            }
            self.island.update(cx, |island, cx| {
                if gone {
                    // Answered elsewhere (`arakun agent approve`), or expired.
                    island.retain_alerts(
                        |a| {
                            a.kind != AlertKind::Confirm
                                || pending.contains(&(a.key.to_string(), a.req.to_string()))
                        },
                        cx,
                    );
                }
                for alert in alerts {
                    island.alert(alert, cx);
                }
            });
            if changed {
                self.push(cx);
            }
        }

        /// An agent outside Arakun changed: its row, and a card when its turn
        /// ended or it waits for the user.
        #[cfg(windows)]
        fn session(&mut self, update: super::external::Update, cx: &mut Context<Self>) -> Reply {
            let card = self.external.update(update, crate::model::unix_now());
            self.push(cx);
            if let Some(card) = card {
                self.island.update(cx, |island, cx| island.alert(card, cx));
            }
            Reply::ok("")
        }

        fn push(&mut self, cx: &mut Context<Self>) {
            let feed = watch::merge(self.app.as_ref().map(|a| &a.feed), &self.headless);
            // The app's choice wins while it is connected; then the system's.
            let choice = match feed.reduce_motion {
                Some(true) => motion::Reduce::On,
                Some(false) => motion::Reduce::Off,
                None => motion::Reduce::System,
            };
            if choice != motion::choice() {
                motion::set_choice(choice, cx);
            }
            #[cfg(windows)]
            let feed = super::external::merge(feed, &mut self.external);
            self.island
                .update(cx, |island, cx| island.set_feed(feed, cx));
        }

        fn on_island(&mut self, ev: &IslandEvent, cx: &mut Context<Self>) {
            if !matches!(ev, IslandEvent::Pressed | IslandEvent::Dismissed) {
                self.button_at = Some(Instant::now());
            }
            match ev {
                // An agent outside Arakun: the app has nothing to show for it.
                #[cfg(windows)]
                IslandEvent::Open(key) | IslandEvent::OpenRow(key)
                    if key.starts_with(super::external::PREFIX) => {}
                IslandEvent::Open(key) => self.show_in_app(FromIsland::Ver { key: key.clone() }),
                IslandEvent::OpenRow(key) => self.show_in_app(FromIsland::Row { key: key.clone() }),
                IslandEvent::Later(key) => {
                    if let Some(app) = &self.app {
                        let _ = app.out.send(FromIsland::Luego { key: key.clone() });
                    }
                }
                IslandEvent::Answer { key, req, approve } => match answer(key, req, *approve) {
                    Ok(done) => eprintln!("arakun: island: {done}"),
                    Err(e) => eprintln!("arakun: island: {e}"),
                },
                IslandEvent::Pressed => {
                    let just_pressed = self.button_at.is_some_and(|at| at.elapsed() < AFTER_BUTTON);
                    if self.app.is_none()
                        && !just_pressed
                        && self.island.read(cx).phase() != Phase::Alert
                    {
                        self.launch();
                    }
                }
                IslandEvent::Dismissed => {}
                #[cfg(feature = "coderlabs")]
                IslandEvent::Face => {
                    if self.island.read(cx).phase() != Phase::Alert {
                        self.voice(None, cx);
                    }
                }
                #[cfg(not(feature = "coderlabs"))]
                IslandEvent::Face => {}
            }
        }

        /// Starts or ends the call; `None` flips it.
        #[cfg(feature = "coderlabs")]
        fn voice(&mut self, on: Option<bool>, cx: &mut Context<Self>) -> Reply {
            let on = on.unwrap_or(self.call.is_none());
            match (on, self.call.take()) {
                (true, Some(c)) => {
                    self.call = Some(c);
                    Reply::ok("la llamada ya está en curso")
                }
                (true, None) => {
                    self.open_call(None, cx);
                    Reply::ok("llamada iniciada")
                }
                (false, Some((_, c))) => {
                    c.stop();
                    self.island
                        .update(cx, |island, cx| island.set_call(None, cx));
                    Reply::ok("llamada terminada")
                }
                (false, None) => Reply::ok("no hay llamada"),
            }
        }

        #[cfg(feature = "coderlabs")]
        fn meeting(
            &mut self,
            on: Option<bool>,
            people: Option<u32>,
            cx: &mut Context<Self>,
        ) -> Reply {
            match (on.unwrap_or(self.meeting.is_none()), self.meeting.take()) {
                (true, Some(m)) => {
                    let dir = m.dir.display().to_string();
                    self.meeting = Some(m);
                    Reply::ok(format!("ya estoy grabando en {dir}"))
                }
                (true, None) => match super::meeting::start(self.meeting_levels.clone()) {
                    Ok(m) => {
                        let dir = m.dir.display().to_string();
                        self.meeting = Some(m);
                        let levels = self.meeting_levels.clone();
                        self.island
                            .update(cx, |island, cx| island.set_recording(Some(levels), cx));
                        Reply::ok(format!("grabando la reunión en {dir}"))
                    }
                    Err(e) => Reply::err(e),
                },
                (false, Some(m)) => {
                    let dir = m.dir.display().to_string();
                    self.island
                        .update(cx, |island, cx| island.set_recording(None, cx));
                    m.finish(people, |done| {
                        // From another thread: through the socket, like `arakun island` would.
                        let (kind, title, sub) = match &done {
                            Ok(dir) => (
                                AlertKind::Success,
                                "Reunión lista",
                                dir.join("resumen.md").display().to_string(),
                            ),
                            Err(e) => (
                                AlertKind::Failure,
                                "La reunión no se pudo procesar",
                                e.clone(),
                            ),
                        };
                        eprintln!("arakun: reunión: {title}: {sub}");
                        let alert = Alert {
                            key: SharedString::default(),
                            kind,
                            title: title.into(),
                            subtitle: sub.into(),
                            req: SharedString::default(),
                            image: SharedString::default(),
                            brand: SharedString::default(),
                        };
                        if let Err(e) = proto::request(&ToIsland::Alert { alert }) {
                            eprintln!("arakun: reunión: {e}");
                        }
                    });
                    Reply::ok(format!(
                        "reunión terminada; la transcribo y te aviso ({dir})"
                    ))
                }
                (false, None) => Reply::ok("no estoy grabando ninguna reunión"),
            }
        }

        /// A new call; with `first`, it opens to report on that agent.
        /// "Oye Arakun": the call opens on the first guess, so it feels instant,
        /// and only goes out once Whisper confirms it; a wrong guess closes quietly.
        #[cfg(feature = "coderlabs")]
        fn wake(&mut self, w: super::wake::Wake, cx: &mut Context<Self>) {
            use super::wake::Wake;
            match (w, &self.call) {
                (Wake::Maybe, None) => {
                    self.open(None, true, cx);
                    self.tentative = true;
                }
                (Wake::Yes(until), Some((_, c))) if self.tentative => {
                    c.confirm(until);
                    self.tentative = false;
                }
                (Wake::Yes(_), None) => self.open_call(None, cx),
                (Wake::No, Some((_, c))) if self.tentative => {
                    c.stop();
                    self.call = None;
                    self.tentative = false;
                    self.island
                        .update(cx, |island, cx| island.set_call(None, cx));
                }
                _ => {}
            }
        }

        #[cfg(feature = "coderlabs")]
        fn open_call(&mut self, first: Option<Alert>, cx: &mut Context<Self>) {
            self.open(first, false, cx)
        }

        #[cfg(feature = "coderlabs")]
        fn open(&mut self, first: Option<Alert>, tentative: bool, cx: &mut Context<Self>) {
            if let Some((_, old)) = self.call.take() {
                eprintln!("arakun: voz: una llamada nueva reemplaza la anterior");
                old.stop();
            }
            self.tentative = false;
            self.calls += 1;
            let n = self.calls;
            let heard = self.heard.clone();
            self.levels.set_mic(0.);
            self.levels.set_out(0.);
            let c = Call::start(self.levels.clone(), first, tentative, move |out| {
                drop(heard.unbounded_send((n, out)))
            });
            self.call = Some((n, c));
            let levels = self.levels.clone();
            self.island
                .update(cx, |island, cx| island.set_call(Some(levels), cx));
        }

        /// A session's alert, said in the call; notices about anything else
        /// (a deploy, CI, a limit) only show.
        #[cfg(feature = "coderlabs")]
        fn tell_call(&self, alert: &Alert) {
            if let Some((_, c)) = &self.call
                && alert.kind.session()
            {
                c.alert(alert);
            }
        }

        #[cfg(feature = "coderlabs")]
        fn on_call(&mut self, n: u64, out: call::Out, cx: &mut Context<Self>) {
            if self.call.as_ref().is_none_or(|(m, _)| *m != n) {
                return;
            }
            match out {
                call::Out::Phase(phase) => self
                    .island
                    .update(cx, |island, cx| island.call_phase(phase, cx)),
                call::Out::Captions { user, assistant } => self
                    .island
                    .update(cx, |island, cx| island.captions(&user, &assistant, cx)),
                call::Out::Open(Some(key)) => self.show_in_app(FromIsland::Ver { key: key.into() }),
                call::Out::Open(None) => match &self.app {
                    Some(app) => focus(app.pid),
                    None => self.launch(),
                },
                call::Out::Ended(error) => {
                    self.call = None;
                    self.island.update(cx, |island, cx| {
                        island.set_call(None, cx);
                        if let Some(e) = error {
                            let subtitle = island::one_line(&e);
                            island.alert(
                                Alert {
                                    key: SharedString::default(),
                                    kind: AlertKind::Failure,
                                    title: "La llamada falló".into(),
                                    subtitle: subtitle.into(),
                                    req: SharedString::default(),
                                    image: SharedString::default(),
                                    brand: SharedString::default(),
                                },
                                cx,
                            );
                        }
                    });
                }
            }
        }

        /// A session to show: in the app, brought to the front, or the app started.
        fn show_in_app(&mut self, ev: FromIsland) {
            match &self.app {
                Some(app) => {
                    let _ = app.out.send(ev);
                    focus(app.pid);
                }
                None => self.launch(),
            }
        }

        fn launch(&mut self) {
            if self.launched.is_some_and(|at| at.elapsed() < LAUNCH_EVERY) {
                return;
            }
            self.launched = Some(Instant::now());
            let started = std::env::current_exe().and_then(|exe| {
                let exe = exe.to_string_lossy();
                let mut cmd =
                    std::process::Command::new(exe.strip_suffix(" (deleted)").unwrap_or(&exe));
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                crate::platform::detach(&mut cmd).spawn()
            });
            match started {
                Ok(mut child) => {
                    eprintln!("arakun: island: started arakun");
                    std::thread::spawn(move || child.wait());
                }
                Err(e) => eprintln!("arakun: island: could not start arakun: {e}"),
            }
        }
    }

    /// The app can't take the focus from another process's click; Hyprland
    /// can give it, and so can macOS.
    fn focus(pid: u32) {
        island::activate_app(pid);
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
            return;
        }
        let run = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("pid:{pid}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut child) = run {
            std::thread::spawn(move || child.wait());
        }
    }

    /// "Approve" / "Deny": the answer `arakun agent approve|deny` writes.
    pub(super) fn answer(key: &str, req: &str, approve: bool) -> Result<String, String> {
        crate::headless::resolve(key, req, approve, "")
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use crate::headless::{self, Answer};

    /// Runs `body` in a child test process whose data folder is `dir`, so
    /// `headless::root()` is a temp folder without touching the others' env.
    fn in_child(name: &str, dir: &std::path::Path) -> bool {
        if std::env::var_os("ARAKUN_ISLAND_TEST_CHILD").is_some() {
            return true;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--test-threads=1"])
            .env("ARAKUN_ISLAND_TEST_CHILD", "1")
            .env("XDG_DATA_HOME", dir)
            .status()
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
        assert!(status.success());
        false
    }

    #[test]
    fn aprobar_and_rechazar_write_the_answer() {
        let name = "island_host::tests::aprobar_and_rechazar_write_the_answer";
        let dir = std::env::temp_dir().join(format!("arakun-island-test-{}", std::process::id()));
        if !in_child(name, &dir) {
            return;
        }
        assert!(headless::root().starts_with(std::env::var_os("XDG_DATA_HOME").unwrap()));
        // A host stand-in: `alive` wants "agent-host" in its command line.
        let mut host = std::process::Command::new("bash")
            .args(["-c", "exec -a arakun-agent-host sleep 30"])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let write = |id: &str, req: &str| {
            let now = crate::model::unix_now();
            let d = headless::dir(id);
            std::fs::create_dir_all(&d).unwrap();
            let meta = serde_json::json!({
                "id": id, "agent": "workers-ai", "model": "workers-ai/@cf/zai-org/glm-5.3", "policy": "workspace",
                "cwd": "/tmp", "title": "t", "created": now, "updated": now, "status": "needs_input", "pid": host.id(),
            });
            std::fs::write(d.join("meta.json"), meta.to_string()).unwrap();
            let pending = serde_json::json!({
                "req_id": req, "session": id, "tool": "execute", "title": "git push", "level": "always",
                "why": "git push publica commits en el remoto", "summary": "Hacer git push de main a origin",
                "phrase": "confirm: git push", "created": now, "expires": now + 60,
            });
            std::fs::write(d.join("pending.json"), pending.to_string()).unwrap();
        };
        write("h1", "r1");
        write("h2", "r2");
        let scan = super::watch::scan();
        let seen = super::watch::Watch::default().update(&scan);
        assert_eq!(seen.alerts.len(), 2);
        assert!(
            seen.alerts
                .iter()
                .all(|a| a.kind == crate::island::AlertKind::Confirm)
        );
        super::host::answer("h1", "r1", true).unwrap();
        super::host::answer("h2", "r2", false).unwrap();
        assert_eq!(
            headless::read_pending("h1").unwrap().answer,
            Some(Answer::Approved)
        );
        assert_eq!(
            headless::read_pending("h2").unwrap().answer,
            Some(Answer::Denied)
        );
        // Answered once only.
        assert!(super::host::answer("h1", "r1", false).is_err());
        assert!(
            super::watch::scan()
                .sessions
                .iter()
                .all(|(_, _, p)| p.is_none())
        );
        let _ = host.kill();
        let _ = host.wait();
    }
}
