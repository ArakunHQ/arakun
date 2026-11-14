//! The PTY + emulator half of a terminal: owns alacritty's `Term` and its I/O thread.
//!
//! Backpressure: alacritty's reader thread parses PTY output straight into the grid.
//! It only ever tells the UI "something changed" through `Shared::dirty`; the first
//! wakeup after a frame sends one message, every later one is dropped until the view
//! paints again and clears the flag. So `yes` costs the UI at most one notify per
//! frame, and a terminal that is not being painted costs nothing at all.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point as AlacPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::{ClipboardType, Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, SharedString, Task};

use super::colors;

pub type AlacTerm = Term<Listener>;

/// Lines kept above the screen.
const SCROLLBACK: usize = 5_000;

#[cfg(unix)]
/// How long what runs in a closed terminal gets to exit after each signal.
const HANGUP_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// `title()` changed (OSC 0/2 or the foreground process).
    TitleChanged,
    /// The child exited; `has_exited()` has the code.
    Exited,
    Bell,
    /// New output. Sent at most once per painted frame, and once in total while the
    /// terminal is not painted, so it doubles as an "unseen output" signal.
    Activity,
}

/// State the I/O thread and the UI share without taking the terminal lock.
struct Shared {
    /// A wakeup is pending or the latest output has not been painted yet.
    dirty: AtomicBool,
    title_pending: AtomicBool,
    bell_pending: AtomicBool,
    title: Mutex<Option<String>>,
    window_size: Mutex<WindowSize>,
    sender: OnceLock<EventLoopSender>,
}

impl Shared {
    fn write(&self, bytes: Cow<'static, [u8]>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(bytes));
        }
    }
}

/// Runs on alacritty's reader thread, while it holds the terminal lock.
#[derive(Clone)]
pub struct Listener {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<AlacEvent>,
}

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        let flag = |f: &AtomicBool| !f.swap(true, Ordering::AcqRel);
        match event {
            AlacEvent::Wakeup => {
                if flag(&self.shared.dirty) {
                    let _ = self.tx.unbounded_send(AlacEvent::Wakeup);
                }
            }
            AlacEvent::PtyWrite(text) => self.shared.write(Cow::Owned(text.into_bytes())),
            AlacEvent::TextAreaSizeRequest(format) => {
                let size = *self.shared.window_size.lock().unwrap();
                self.shared.write(Cow::Owned(format(size).into_bytes()));
            }
            AlacEvent::Title(title) => {
                *self.shared.title.lock().unwrap() = Some(title);
                if flag(&self.shared.title_pending) {
                    let _ = self.tx.unbounded_send(AlacEvent::ResetTitle);
                }
            }
            AlacEvent::ResetTitle => {
                *self.shared.title.lock().unwrap() = None;
                if flag(&self.shared.title_pending) {
                    let _ = self.tx.unbounded_send(AlacEvent::ResetTitle);
                }
            }
            AlacEvent::Bell => {
                if flag(&self.shared.bell_pending) {
                    let _ = self.tx.unbounded_send(AlacEvent::Bell);
                }
            }
            AlacEvent::MouseCursorDirty | AlacEvent::CursorBlinkingChange => {}
            other => {
                let _ = self.tx.unbounded_send(other);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct Terminal {
    term: Arc<FairMutex<AlacTerm>>,
    shared: Arc<Shared>,
    pid: u32,
    /// For the foreground process group, which only Unix PTYs have.
    #[cfg(unix)]
    pty_fd: i32,
    program: String,
    cwd: PathBuf,
    osc_title: Option<String>,
    process_name: Option<String>,
    process_checked: Option<Instant>,
    exit_code: Option<i32>,
    size: WindowSize,
    hung_up: AtomicBool,
    _events: Task<()>,
}

impl EventEmitter<TerminalEvent> for Terminal {}

struct Spawned {
    term: Arc<FairMutex<AlacTerm>>,
    shared: Arc<Shared>,
    rx: mpsc::UnboundedReceiver<AlacEvent>,
    pid: u32,
    #[cfg(unix)]
    pty_fd: i32,
    program: String,
    cwd: PathBuf,
    size: WindowSize,
}

fn spawn_pty(cwd: PathBuf, shell_command: Option<Vec<String>>) -> anyhow::Result<Spawned> {
    let (program, args) = match shell_command {
        Some(mut cmd) if !cmd.is_empty() => {
            let program = cmd.remove(0);
            (program, cmd)
        }
        _ => {
            let mut shell = crate::platform::default_shell();
            let program = shell.remove(0);
            (program, shell)
        }
    };

    let env = [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "arakun"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let options = tty::Options {
        shell: Some(tty::Shell::new(program.clone(), args)),
        working_directory: Some(cwd.clone()),
        drain_on_exit: true,
        env,
        // C runtime quoting, which PowerShell and everything else parse back intact.
        #[cfg(windows)]
        escape_args: true,
    };

    let size = WindowSize {
        num_lines: 24,
        num_cols: 80,
        cell_width: 8,
        cell_height: 17,
    };
    let pty = tty::new(&options, size, 0)?;
    #[cfg(unix)]
    let (pid, pty_fd) = {
        use std::os::fd::AsRawFd as _;
        (pty.child().id(), pty.file().as_raw_fd())
    };
    #[cfg(windows)]
    let pid = pty.child_watcher().pid().map_or(0, |p| p.get());

    let shared = Arc::new(Shared {
        dirty: AtomicBool::new(false),
        title_pending: AtomicBool::new(false),
        bell_pending: AtomicBool::new(false),
        title: Mutex::new(None),
        window_size: Mutex::new(size),
        sender: OnceLock::new(),
    });
    let (tx, rx) = mpsc::unbounded();
    let listener = Listener {
        shared: shared.clone(),
        tx,
    };

    let config = Config {
        scrolling_history: SCROLLBACK,
        default_cursor_style: CursorStyle {
            shape: CursorShape::Beam,
            blinking: true,
        },
        kitty_keyboard: false,
        ..Config::default()
    };
    let grid = GridSize {
        columns: size.num_cols as usize,
        lines: size.num_lines as usize,
    };
    let term = Arc::new(FairMutex::new(Term::new(config, &grid, listener.clone())));

    let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
    let _ = shared.sender.set(event_loop.channel());
    event_loop.spawn();

    Ok(Spawned {
        term,
        shared,
        rx,
        pid,
        #[cfg(unix)]
        pty_fd,
        program,
        cwd,
        size,
    })
}

impl Terminal {
    /// Spawns `shell_command` (or the user's login shell) in a new PTY at `cwd`.
    #[allow(dead_code)]
    pub fn new(
        cwd: PathBuf,
        shell_command: Option<Vec<String>>,
        cx: &mut Context<Terminal>,
    ) -> anyhow::Result<Terminal> {
        Ok(Self::from_spawned(spawn_pty(cwd, shell_command)?, cx))
    }

    /// `new` for callers outside an entity context: spawn errors come back before an
    /// entity exists, which `cx.new` can't express.
    pub fn spawn(
        cwd: PathBuf,
        shell_command: Option<Vec<String>>,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Terminal>> {
        let spawned = spawn_pty(cwd, shell_command)?;
        Ok(cx.new(|cx| Self::from_spawned(spawned, cx)))
    }

    fn from_spawned(spawned: Spawned, cx: &mut Context<Terminal>) -> Terminal {
        let Spawned {
            term,
            shared,
            mut rx,
            pid,
            #[cfg(unix)]
            pty_fd,
            program,
            cwd,
            size,
        } = spawned;
        let events = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(next) = rx.try_recv() {
                    batch.push(next);
                }
                let alive = this.update(cx, |this, cx| {
                    for event in batch {
                        this.handle_event(event, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        Terminal {
            term,
            shared,
            pid,
            #[cfg(unix)]
            pty_fd,
            program,
            cwd,
            osc_title: None,
            process_name: None,
            process_checked: None,
            exit_code: None,
            size,
            hung_up: AtomicBool::new(false),
            _events: events,
        }
    }

    fn handle_event(&mut self, event: AlacEvent, cx: &mut Context<Self>) {
        match event {
            AlacEvent::Wakeup => {
                self.refresh_process_name(cx);
                cx.emit(TerminalEvent::Activity);
                cx.notify();
            }
            AlacEvent::ResetTitle => {
                self.shared.title_pending.store(false, Ordering::Release);
                let title = self
                    .shared
                    .title
                    .lock()
                    .unwrap()
                    .clone()
                    .filter(|t| !t.trim().is_empty());
                if title != self.osc_title {
                    self.osc_title = title;
                    cx.emit(TerminalEvent::TitleChanged);
                    cx.notify();
                }
            }
            AlacEvent::Bell => {
                self.shared.bell_pending.store(false, Ordering::Release);
                cx.emit(TerminalEvent::Bell);
            }
            AlacEvent::ChildExit(status) => {
                #[cfg(unix)]
                let signal = {
                    use std::os::unix::process::ExitStatusExt as _;
                    status.signal().map(|s| 128 + s)
                };
                #[cfg(not(unix))]
                let signal = None;
                let code = status.code().or(signal).unwrap_or(-1);
                self.set_exited(code, cx);
            }
            AlacEvent::Exit => self.set_exited(0, cx),
            AlacEvent::ClipboardStore(ty, text) => {
                let item = ClipboardItem::new_string(text);
                match ty {
                    ClipboardType::Clipboard => cx.write_to_clipboard(item),
                    ClipboardType::Selection => crate::platform::write_primary(cx, item),
                }
            }
            AlacEvent::ColorRequest(index, format) => {
                let rgb = self.term.lock().colors()[index].unwrap_or_else(|| colors::default_rgb(index));
                self.shared.write(Cow::Owned(format(rgb).into_bytes()));
            }
            _ => {}
        }
    }

    fn set_exited(&mut self, code: i32, cx: &mut Context<Self>) {
        if self.exit_code.is_none() {
            self.exit_code = Some(code);
            self.process_name = None;
            cx.emit(TerminalEvent::Exited);
            cx.emit(TerminalEvent::TitleChanged);
            cx.notify();
        }
    }

    /// Name of the foreground process (e.g. "cargo" while `cargo run` runs), checked at
    /// most twice a second and only when output arrives.
    fn refresh_process_name(&mut self, cx: &mut Context<Self>) {
        if self.exit_code.is_some()
            || self
                .process_checked
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        self.process_checked = Some(Instant::now());
        let name = self.foreground_name();
        if name != self.process_name {
            self.process_name = name;
            if self.osc_title.is_none() {
                cx.emit(TerminalEvent::TitleChanged);
            }
        }
    }

    #[cfg(unix)]
    fn foreground_name(&self) -> Option<String> {
        // SAFETY: tcgetpgrp only reads the descriptor, which the PTY keeps open.
        let pgrp = unsafe { libc::tcgetpgrp(self.pty_fd) };
        (pgrp > 0)
            .then(|| std::fs::read_to_string(format!("/proc/{pgrp}/comm")).ok())
            .flatten()
            .map(|s| s.trim().to_string())
    }

    /// ConPTY has no foreground process group to ask; the shell's own title (PowerShell
    /// sets one) or its name stands in.
    #[cfg(not(unix))]
    fn foreground_name(&self) -> Option<String> {
        None
    }

    /// OSC title, else the foreground process, else the program name.
    pub fn title(&self) -> SharedString {
        if let Some(title) = &self.osc_title {
            return title.clone().into();
        }
        if let Some(name) = &self.process_name {
            return name.clone().into();
        }
        basename(Path::new(&self.program))
            .or_else(|| basename(&self.cwd))
            .unwrap_or_else(|| "terminal".into())
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn has_exited(&self) -> Option<i32> {
        self.exit_code
    }

    /// Raw bytes to the PTY (no bracketed paste, no scroll reset).
    pub fn write_input(&self, bytes: &[u8]) {
        self.shared.write(Cow::Owned(bytes.to_vec()));
    }

    /// Sends text the way a paste does: bracketed when the program asked for it,
    /// newlines as carriage returns otherwise.
    pub fn paste(&self, text: &str) {
        let bytes = if self.mode().contains(TermMode::BRACKETED_PASTE) {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r")
        };
        self.scroll_to_bottom();
        self.write_input(bytes.as_bytes());
    }

    /// Hangs up the shell (SIGHUP, like closing a terminal window) and
    /// everything else still running in its session. Once the shell has
    /// exited on its own, what it left behind (`cmd & disown`) is left alone,
    /// as other terminals do.
    pub fn kill(&self) {
        if self.exit_code.is_none() && !self.hung_up.swap(true, Ordering::AcqRel) {
            #[cfg(unix)]
            {
                // SAFETY: plain signal delivery to the shell we started.
                unsafe {
                    libc::kill(self.pid as i32, libc::SIGHUP);
                }
                end_session(self.pid as i32, HANGUP_GRACE);
            }
            // ConPTY has no sessions; closing the console ends what is attached to it.
            #[cfg(windows)]
            crate::platform::hang_up(self.pid);
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub(crate) fn term(&self) -> &Arc<FairMutex<AlacTerm>> {
        &self.term
    }

    /// The view calls this right before it reads the grid for a frame.
    pub(crate) fn begin_frame(&self) {
        self.shared.dirty.store(false, Ordering::Release);
    }

    #[allow(dead_code)]
    pub fn grid_size(&self) -> (usize, usize) {
        (self.size.num_cols as usize, self.size.num_lines as usize)
    }

    /// Resizes grid and PTY. `term` must be the locked terminal.
    pub(crate) fn resize_locked(
        &mut self,
        term: &mut AlacTerm,
        columns: usize,
        lines: usize,
        cell_width: f32,
        cell_height: f32,
    ) {
        let size = WindowSize {
            num_lines: lines.clamp(1, u16::MAX as usize) as u16,
            num_cols: columns.clamp(2, u16::MAX as usize) as u16,
            cell_width: cell_width.round() as u16,
            cell_height: cell_height.round() as u16,
        };
        if size.num_lines == self.size.num_lines
            && size.num_cols == self.size.num_cols
            && size.cell_width == self.size.cell_width
            && size.cell_height == self.size.cell_height
        {
            return;
        }
        let grid_changed =
            size.num_lines != self.size.num_lines || size.num_cols != self.size.num_cols;
        self.size = size;
        *self.shared.window_size.lock().unwrap() = size;
        if grid_changed {
            term.resize(GridSize {
                columns: size.num_cols as usize,
                lines: size.num_lines as usize,
            });
        }
        if let Some(sender) = self.shared.sender.get() {
            let _ = sender.send(Msg::Resize(size));
        }
    }

    pub fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub(crate) fn start_selection(&self, ty: SelectionType, point: AlacPoint, side: Side) {
        self.term.lock().selection = Some(Selection::new(ty, point, side));
    }

    pub(crate) fn update_selection(&self, point: AlacPoint, side: Side) {
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Text of the logical line (following wraps) around `point`, plus the column of
    /// `point` within it, used to find URLs under the mouse.
    pub(crate) fn line_around(&self, point: AlacPoint) -> (String, usize) {
        use alacritty_terminal::term::cell::Flags;
        let term = self.term.lock();
        let grid = term.grid();
        let top = grid.topmost_line();
        let bottom = grid.bottommost_line();
        let cols = grid.columns();
        let mut start = point.line;
        while start > top && grid[Line(start.0 - 1)][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            start -= 1;
        }
        let mut end = point.line;
        while end < bottom && grid[end][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            end += 1;
        }
        let mut text = String::new();
        let mut index = 0;
        let mut line = start;
        while line <= end {
            for col in 0..cols {
                let cell = &grid[line][Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if line == point.line && col == point.column.0 {
                    index = text.chars().count();
                }
                text.push(cell.c);
            }
            line += 1;
        }
        (text, index)
    }

    /// Finds `query` as literal text (case-insensitive unless it has capitals)
    /// starting next to `after` (else from the bottom of the scrollback), going
    /// up (`older`) or down; wraps around. Selects the match and scrolls to it.
    pub fn find(&self, query: &str, after: Option<&Match>, older: bool) -> Option<Match> {
        find_in(&mut self.term.lock(), query, after, older)
    }

    /// The last `max_lines` lines of output (scrollback included), wrapped
    /// lines joined back, trailing blanks dropped.
    pub fn recent_text(&self, max_lines: usize) -> String {
        tail_text(&self.term.lock(), max_lines)
    }

    /// OSC 8 hyperlink at `point`, if any.
    pub(crate) fn hyperlink_at(&self, point: AlacPoint) -> Option<String> {
        let term = self.term.lock();
        let grid = term.grid();
        if point.line < grid.topmost_line() || point.line > grid.bottommost_line() {
            return None;
        }
        grid[point]
            .hyperlink()
            .map(|link| link.uri().to_string())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // The reader thread exits, drops the PTY (SIGHUP to the child) and reaps it.
        if let Some(sender) = self.shared.sender.get() {
            let _ = sender.send(Msg::Shutdown);
        }
        self.kill();
    }
}

#[cfg(unix)]
/// Ends what runs in a terminal's session (`sid`, its shell's pid). Closing
/// the PTY only hangs up the foreground job, so background jobs and programs
/// in their own process group (`timeout`, dev servers started with `&`)
/// would outlive the tab. Every member gets SIGHUP; whatever is still there
/// after `grace` and doesn't ignore SIGHUP (`nohup`) gets SIGTERM, then
/// SIGKILL. Programs that left the session (`setsid`, tmux) are not touched.
/// Runs on its own thread.
fn end_session(sid: i32, grace: Duration) {
    let _ = std::thread::Builder::new().name("term-hangup".into()).spawn(move || hang_up(sid, grace));
}

#[cfg(unix)]
fn hang_up(sid: i32, grace: Duration) {
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGKILL] {
        let targets: Vec<i32> = session_members(sid)
            .into_iter()
            .filter(|&(_, ignores_hup)| signal == libc::SIGHUP || !ignores_hup)
            .map(|(pid, _)| pid)
            .collect();
        if targets.is_empty() {
            return;
        }
        for pid in targets {
            unsafe { libc::kill(pid, signal) };
        }
        if signal != libc::SIGKILL {
            std::thread::sleep(grace);
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
/// Live processes of session `sid`, each with whether it ignores SIGHUP.
fn session_members(sid: i32) -> Vec<(i32, bool)> {
    let ours = std::process::id() as i32;
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid != ours)
        .filter(|&pid| {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            // After the command name: state, ppid, pgrp, session.
            let fields: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, f)| f).split_whitespace().collect();
            fields.first().is_some_and(|&s| s != "Z") && fields.get(3).and_then(|s| s.parse().ok()) == Some(sid)
        })
        .map(|pid| (pid, ignores_hup(pid)))
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn ignores_hup(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            let mask = s.lines().find_map(|l| l.strip_prefix("SigIgn:"))?;
            u64::from_str_radix(mask.trim(), 16).ok()
        })
        .is_some_and(|mask| mask & (1 << (libc::SIGHUP - 1)) != 0)
}

// macOS has no /proc. The pids come from `proc_listallpids`, the session
// from `getsid`, and the state and ignored signals from
// `sysctl(KERN_PROC_PID)`; `proc_pidinfo` doesn't report ignored signals.
#[cfg(target_os = "macos")]
fn session_members(sid: i32) -> Vec<(i32, bool)> {
    let ours = std::process::id() as i32;
    // With no buffer it returns how many pids there are; ask for some extra
    // room in case processes start in between.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(count.max(0) as usize);
    pids.into_iter()
        .filter(|&pid| pid > 0 && pid != ours && unsafe { libc::getsid(pid) } == sid)
        .filter_map(|pid| {
            let proc = extern_proc(pid)?;
            (u32::from(proc.p_stat) != libc::SZOMB).then(|| (pid, proc_ignores_hup(&proc)))
        })
        .collect()
}

// Only the tests ask about a single pid on macOS.
#[cfg(all(target_os = "macos", test))]
fn ignores_hup(pid: i32) -> bool {
    extern_proc(pid).is_some_and(|proc| proc_ignores_hup(&proc))
}

#[cfg(target_os = "macos")]
fn proc_ignores_hup(proc: &ExternProc) -> bool {
    proc.p_sigignore & (1 << (libc::SIGHUP - 1)) != 0
}

#[cfg(target_os = "macos")]
fn extern_proc(pid: i32) -> Option<ExternProc> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut info = std::mem::MaybeUninit::<KinfoProc>::zeroed();
    let mut len = std::mem::size_of::<KinfoProc>();
    let ok = unsafe {
        libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, info.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    } == 0;
    // A pid that is gone succeeds with nothing written.
    (ok && len == std::mem::size_of::<KinfoProc>()).then(|| unsafe { info.assume_init() }.kp_proc)
}

// libc doesn't declare `struct kinfo_proc` for macOS. This copies
// `struct extern_proc` from <sys/proc.h> with pointers as `usize`, and keeps
// `struct eproc` as bytes since nothing here reads it. The sizes are those of
// the 64-bit SDK; the asserts catch a field out of place.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct KinfoProc {
    kp_proc: ExternProc,
    kp_eproc: [u8; 352],
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct ExternProc {
    p_starttime: [u64; 2],
    p_vmspace: usize,
    p_sigacts: usize,
    p_flag: i32,
    p_stat: u8,
    p_pid: i32,
    p_oppid: i32,
    p_dupfd: i32,
    user_stack: usize,
    exit_thread: usize,
    p_debugger: i32,
    sigwait: i32,
    p_estcpu: u32,
    p_cpticks: i32,
    p_pctcpu: u32,
    p_wchan: usize,
    p_wmesg: usize,
    p_swtime: u32,
    p_slptime: u32,
    p_realtimer: [u64; 4],
    p_rtime: [u64; 2],
    p_uticks: u64,
    p_sticks: u64,
    p_iticks: u64,
    p_traceflag: i32,
    p_tracep: usize,
    p_siglist: i32,
    p_textvp: usize,
    p_holdcnt: i32,
    p_sigmask: u32,
    p_sigignore: u32,
    p_sigcatch: u32,
    p_priority: u8,
    p_usrpri: u8,
    p_nice: i8,
    p_comm: [u8; 17],
    p_pgrp: usize,
    p_addr: usize,
    p_xstat: u16,
    p_acflag: u16,
    p_ru: usize,
}

#[cfg(target_os = "macos")]
const _: () = {
    assert!(std::mem::size_of::<ExternProc>() == 296);
    assert!(std::mem::offset_of!(ExternProc, p_sigignore) == 232);
    assert!(std::mem::size_of::<KinfoProc>() == 648);
};

fn find_in<T: EventListener>(
    term: &mut Term<T>,
    query: &str,
    after: Option<&Match>,
    older: bool,
) -> Option<Match> {
    if query.is_empty() {
        return None;
    }
    let escaped: String = query
        .chars()
        .flat_map(|c| {
            let special = "\\.+*?()|[]{}^$#&-~".contains(c);
            special.then_some('\\').into_iter().chain(std::iter::once(c))
        })
        .collect();
    let mut regex = RegexSearch::new(&escaped).ok()?;
    let (direction, side) = if older {
        (Direction::Left, Side::Left)
    } else {
        (Direction::Right, Side::Right)
    };
    let origin = match after {
        Some(m) if older => m.start().sub(&*term, Boundary::Grid, 1),
        Some(m) => m.end().add(&*term, Boundary::Grid, 1),
        None => {
            let grid = term.grid();
            AlacPoint::new(grid.bottommost_line(), grid.last_column())
        }
    };
    let found = term.search_next(&mut regex, origin, direction, side, None)?;
    let mut selection = Selection::new(SelectionType::Simple, *found.start(), Side::Left);
    selection.update(*found.end(), Side::Right);
    term.selection = Some(selection);
    term.scroll_to_point(*found.start());
    Some(found)
}

fn tail_text<T>(term: &Term<T>, max_lines: usize) -> String {
    use alacritty_terminal::term::cell::Flags;
    let grid = term.grid();
    let cols = grid.columns();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut line = grid.topmost_line();
    while line <= grid.bottommost_line() {
        let row = &grid[line];
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            current.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                current.extend(extra);
            }
        }
        if !row[Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            lines.push(current.trim_end().to_string());
            current.clear();
        }
        line += 1;
    }
    if !current.is_empty() {
        lines.push(current.trim_end().to_string());
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn basename(path: &Path) -> Option<SharedString> {
    path.file_name()
        .map(|n| SharedString::from(n.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    #[test]
    fn recent_text_joins_wrapped_lines() {
        let mut term = Term::new(Config::default(), &TermSize::new(10, 4), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"one\r\ntwo\r\nthis line wraps around\r\nlast   \r\n");
        assert_eq!(tail_text(&term, 100), "one\ntwo\nthis line wraps around\nlast");
        assert_eq!(tail_text(&term, 2), "this line wraps around\nlast");
    }

    #[test]
    fn finds_literal_text() {
        let mut term = Term::new(Config::default(), &TermSize::new(20, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"a.b one\r\naxb two\r\na.b three");
        let first = find_in(&mut term, "a.b", None, true).unwrap();
        assert_eq!(first.start().line, Line(2));
        let second = find_in(&mut term, "a.b", Some(&first), true).unwrap();
        assert_eq!(second.start().line, Line(0));
        // Wraps around to the newest match again.
        let third = find_in(&mut term, "A.B", Some(&second), true);
        assert!(third.is_none(), "capitals make it case-sensitive");
        assert!(find_in(&mut term, "zzz", None, true).is_none());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn hanging_up_ends_the_session_but_spares_nohup() {
        use std::os::unix::process::CommandExt as _;
        let dir = std::env::temp_dir().join(format!("arakun-hangup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A plain background job, one in its own process group that shrugs
        // off SIGHUP, and a nohup'd one.
        let script = "sleep 60 &\n\
             set -m; (trap : HUP; while :; do sleep 0.1; done) & set +m\n\
             nohup sleep 60 >/dev/null 2>&1 &\n\
             echo $! > nohup.pid\n\
             exec sleep 60";
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", script]).current_dir(&dir);
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut leader = cmd.spawn().unwrap();
        let sid = leader.id() as i32;
        // The pid lands in nohup.pid as soon as the shell forks, while the
        // child is still a plain copy of the shell; it only ignores SIGHUP
        // once `nohup` itself runs. Hanging up before that takes it along.
        let started = Instant::now();
        let spared = loop {
            let pid = std::fs::read_to_string(dir.join("nohup.pid"))
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .filter(|&pid| ignores_hup(pid));
            if let Some(pid) = pid {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "nohup never came up: {:?}", session_members(sid));
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(session_members(sid).len() >= 4, "{:?}", session_members(sid));
        hang_up(sid, Duration::from_millis(300));
        let _ = leader.wait();
        // SIGKILL is delivered asynchronously; the last ones may need a moment
        // to leave /proc.
        let started = Instant::now();
        let left = loop {
            let left: Vec<i32> = session_members(sid).into_iter().map(|(p, _)| p).collect();
            if left == [spared] || started.elapsed() > Duration::from_secs(2) {
                break left;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(left, [spared]);
        unsafe { libc::kill(spared, libc::SIGKILL) };
        let _ = std::fs::remove_dir_all(dir);
    }
}
