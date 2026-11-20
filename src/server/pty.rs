//! A shell on a pseudo-terminal, for a terminal tab a device shows: the
//! server's side of it. The bytes go to the device as they come, and its own
//! emulator draws them. The last [`KEPT`] bytes stay here, so a device that
//! was away gets what it missed when it comes back.
//!
//! One thread per terminal reads and writes the PTY, the way alacritty's own
//! event loop does in the app.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read as _, Write as _};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard};

use alacritty_terminal::event::{OnResize as _, WindowSize};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty as _, EventedReadWrite as _};
use futures::channel::mpsc;
use polling::{Event as PollEvent, Events, PollMode, Poller};

/// The keys alacritty's PTY registers its events under. Unix keeps them to
/// itself, so its values are repeated here.
#[cfg(unix)]
const READ_WRITE: usize = 0;
#[cfg(unix)]
const CHILD_EVENT: usize = 1;
#[cfg(windows)]
const READ_WRITE: usize = tty::PTY_READ_WRITE_TOKEN;
#[cfg(windows)]
const CHILD_EVENT: usize = tty::PTY_CHILD_EVENT_TOKEN;

/// The output kept for devices that fall behind or come back.
pub const KEPT: usize = 256 * 1024;

/// The most one read takes before the thread looks at hang-ups and typing
/// again. A terminal that prints without pause would hold it otherwise. The
/// poller is level-triggered, so what is left wakes it straight away.
const READ_BATCH: usize = 64 * 1024;

/// What a terminal printed, as far back as [`KEPT`] goes, and how it ended.
#[derive(Default)]
pub struct Output {
    bytes: VecDeque<u8>,
    /// The offset of `bytes[0]` in everything the terminal printed.
    start: u64,
    /// The shell ended, with its exit code when it had one.
    pub exit: Option<Option<i32>>,
}

impl Output {
    /// The offset just past the last byte printed.
    pub fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    /// Up to `max` bytes from offset `from` on, and the offset of the first.
    /// That is later than `from` when the bytes there were let go already.
    pub fn since(&self, from: u64, max: usize) -> (u64, Vec<u8>) {
        let at = from.clamp(self.start, self.end());
        let skip = (at - self.start) as usize;
        (
            at,
            self.bytes.iter().skip(skip).take(max).copied().collect(),
        )
    }

    pub(super) fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data);
        let over = self.bytes.len().saturating_sub(KEPT);
        if over > 0 {
            self.bytes.drain(..over);
            self.start += over as u64;
        }
    }
}

enum In {
    Input(Vec<u8>),
    Resize(u16, u16),
}

/// The shell. Dropping it hangs up.
pub struct Pty {
    output: Arc<Mutex<Output>>,
    input: std_mpsc::Sender<In>,
    poller: Arc<Poller>,
    stop: Arc<AtomicBool>,
    /// A wake-up was sent and not yet taken: more output needs no second one.
    woken: Arc<AtomicBool>,
}

impl Pty {
    /// Starts `command`, or the login shell, in `cwd`. `wake` hears `id`
    /// whenever there is new output or the shell ended.
    pub fn spawn(
        id: String,
        cwd: PathBuf,
        command: Option<Vec<String>>,
        (cols, rows): (u16, u16),
        wake: mpsc::UnboundedSender<String>,
    ) -> std::io::Result<Pty> {
        // The common case gets a plain error; `in_dir` covers a folder that
        // goes between this look and the child's start.
        if !cwd.is_dir() {
            return Err(std::io::Error::new(
                ErrorKind::NotFound,
                format!("{} is not a folder", cwd.display()),
            ));
        }
        let mut argv = command
            .filter(|c| !c.is_empty())
            .unwrap_or_else(crate::platform::default_shell);
        #[cfg(unix)]
        {
            argv = in_dir(&cwd, argv)?;
        }
        let program = argv.remove(0);
        let env = [
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "arakun"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let options = tty::Options {
            shell: Some(tty::Shell::new(program, argv)),
            working_directory: Some(cwd),
            drain_on_exit: true,
            env,
            // C runtime quoting, as the app's terminals use.
            #[cfg(windows)]
            escape_args: true,
        };
        let pty = tty::new(&options, window_size(cols, rows), 0)?;
        let poller = Arc::new(Poller::new()?);
        let (input, inputs) = std_mpsc::channel();
        let output = Arc::new(Mutex::new(Output::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let woken = Arc::new(AtomicBool::new(false));
        let run = Run {
            id,
            pty,
            poller: poller.clone(),
            inputs,
            output: output.clone(),
            stop: stop.clone(),
            woken: woken.clone(),
            wake,
            pending: VecDeque::new(),
        };
        std::thread::Builder::new()
            .name("arakun-server-pty".into())
            .spawn(move || run.run())?;
        Ok(Pty {
            output,
            input,
            poller,
            stop,
            woken,
        })
    }

    /// What it printed. Taking it also takes the wake-up: output after this
    /// sends another.
    pub fn output(&self) -> MutexGuard<'_, Output> {
        self.woken.store(false, Ordering::Release);
        self.output.lock().unwrap()
    }

    /// Bytes typed into it.
    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.send(In::Input(bytes));
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.send(In::Resize(cols, rows));
    }

    fn send(&self, msg: In) {
        if self.input.send(msg).is_ok() {
            let _ = self.poller.notify();
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.poller.notify();
    }
}

/// `argv`, run by a `sh` that enters `cwd` first and stops when it cannot.
/// Alacritty ignores a failed `chdir` on Unix, which would run the command
/// in the server's own folder.
#[cfg(unix)]
fn in_dir(cwd: &std::path::Path, argv: Vec<String>) -> std::io::Result<Vec<String>> {
    let dir = cwd.to_str().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{} is not a UTF-8 path", cwd.display()),
        )
    })?;
    let mut wrapped = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"cd -- "$1" || exit 126; shift; exec "$@""#.into(),
        "sh".into(),
        dir.into(),
    ];
    wrapped.extend(argv);
    Ok(wrapped)
}

fn window_size(cols: u16, rows: u16) -> WindowSize {
    WindowSize {
        num_cols: cols.max(2),
        num_lines: rows.max(1),
        cell_width: 8,
        cell_height: 17,
    }
}

/// The thread's side.
struct Run {
    id: String,
    pty: tty::Pty,
    poller: Arc<Poller>,
    inputs: std_mpsc::Receiver<In>,
    output: Arc<Mutex<Output>>,
    stop: Arc<AtomicBool>,
    woken: Arc<AtomicBool>,
    wake: mpsc::UnboundedSender<String>,
    /// Typed bytes the PTY has not taken yet.
    pending: VecDeque<u8>,
}

impl Run {
    fn run(mut self) {
        let mut interest = PollEvent::readable(0);
        // SAFETY: the PTY stays registered with this poller only, and is
        // deregistered before both go.
        if unsafe { self.pty.register(&self.poller, interest, PollMode::Level) }.is_err() {
            self.ended(None);
            return;
        }
        let mut events = Events::with_capacity(NonZeroUsize::new(64).unwrap());
        let mut buf = vec![0u8; 64 * 1024];
        let code = 'run: loop {
            events.clear();
            if let Err(e) = self.poller.wait(&mut events, None)
                && e.kind() != ErrorKind::Interrupted
            {
                break None;
            }
            if self.stop.load(Ordering::Acquire) {
                // Hung up: nobody is left to tell. Dropping the PTY ends the shell.
                let _ = self.pty.deregister(&self.poller);
                return;
            }
            while let Ok(msg) = self.inputs.try_recv() {
                match msg {
                    In::Input(bytes) => self.pending.extend(bytes),
                    In::Resize(cols, rows) => self.pty.on_resize(window_size(cols, rows)),
                }
            }
            for event in events.iter() {
                match event.key {
                    CHILD_EVENT => {
                        if let Some(ChildEvent::Exited(status)) = self.pty.next_child_event() {
                            self.drain(&mut buf);
                            break 'run status.map(exit_code);
                        }
                    }
                    READ_WRITE => {
                        if event.is_interrupt() {
                            continue;
                        }
                        if event.readable
                            && let Err(e) = self.read(&mut buf)
                        {
                            // Linux says EIO once the last process let go of
                            // the terminal; the child's exit follows.
                            #[cfg(target_os = "linux")]
                            if e.raw_os_error() == Some(libc::EIO) {
                                continue;
                            }
                            let _ = e;
                            break 'run None;
                        }
                        if event.writable && self.write().is_err() {
                            break 'run None;
                        }
                    }
                    _ => {}
                }
            }
            // Asked to write, the PTY wakes the poller as soon as it can take more.
            let wants = !self.pending.is_empty();
            if wants != interest.writable {
                interest.writable = wants;
                if self
                    .pty
                    .reregister(&self.poller, interest, PollMode::Level)
                    .is_err()
                {
                    break None;
                }
            }
        };
        let _ = self.pty.deregister(&self.poller);
        self.ended(code);
    }

    /// Reads what there is now, up to [`READ_BATCH`] bytes. True when it
    /// stopped at that limit and more may be waiting.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<bool> {
        let mut got = 0;
        let result = loop {
            if got >= READ_BATCH {
                break Ok(true);
            }
            match self.pty.reader().read(buf) {
                Ok(0) => break Ok(false),
                Ok(n) => {
                    self.output.lock().unwrap().push(&buf[..n]);
                    got += n;
                }
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
                    break Ok(false);
                }
                Err(e) => break Err(e),
            }
        };
        if got > 0 {
            self.wake();
        }
        result
    }

    /// Reads what the shell left behind once it ended, a batch at a time so
    /// a hang-up still stops it.
    fn drain(&mut self, buf: &mut [u8]) {
        while !self.stop.load(Ordering::Acquire) && matches!(self.read(buf), Ok(true)) {}
    }

    fn write(&mut self) -> std::io::Result<()> {
        while !self.pending.is_empty() {
            let (front, _) = self.pending.as_slices();
            match self.pty.writer().write(front) {
                Ok(0) => break,
                Ok(n) => {
                    self.pending.drain(..n);
                }
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn wake(&self) {
        if !self.woken.swap(true, Ordering::AcqRel) {
            let _ = self.wake.unbounded_send(self.id.clone());
        }
    }

    fn ended(&mut self, code: Option<i32>) {
        self.output.lock().unwrap().exit = Some(code);
        // Always told, even mid-wake: the end must not wait for more output.
        self.woken.store(true, Ordering::Release);
        let _ = self.wake.unbounded_send(self.id.clone());
    }
}

/// The exit code, or 128 plus the signal that ended it, as shells say.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_keeps_the_last_bytes_and_their_offsets() {
        let mut out = Output::default();
        out.push(b"hello ");
        out.push(b"world");
        assert_eq!(out.end(), 11);
        assert_eq!(out.since(6, 100), (6, b"world".to_vec()));
        assert_eq!(out.since(0, 5), (0, b"hello".to_vec()));
        assert_eq!(out.since(20, 5), (11, Vec::new()));

        out.push(&vec![b'x'; KEPT]);
        assert_eq!(out.end(), 11 + KEPT as u64);
        // What fell out is skipped: the first byte left comes instead.
        let (at, bytes) = out.since(0, 3);
        assert_eq!(at, 11);
        assert_eq!(bytes, b"xxx");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_runs_and_its_output_and_exit_come_back() {
        use futures::StreamExt as _;
        let (wake, mut woken) = mpsc::unbounded();
        let dir = std::env::temp_dir();
        let cmd = vec!["/bin/sh".into(), "-c".into(), "printf hi; exit 3".into()];
        let pty = Pty::spawn("t1".into(), dir, Some(cmd), (80, 24), wake).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(std::time::Instant::now() < deadline, "no exit");
            let id = futures::executor::block_on(woken.next()).unwrap();
            assert_eq!(id, "t1");
            let out = pty.output();
            if let Some(code) = out.exit {
                let (_, bytes) = out.since(0, 1024);
                assert!(String::from_utf8_lossy(&bytes).contains("hi"));
                assert_eq!(code, Some(3));
                break;
            }
        }
    }

    #[test]
    fn a_missing_folder_is_not_started_in() {
        let (wake, _woken) = mpsc::unbounded();
        let gone = std::env::temp_dir().join("arakun-pty-no-such-folder");
        let cmd = vec!["/bin/sh".into(), "-c".into(), "true".into()];
        assert!(Pty::spawn("t2".into(), gone, Some(cmd), (80, 24), wake).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_command_runs_in_its_folder_or_not_at_all() {
        let run = |dir: &std::path::Path| {
            let argv = in_dir(dir, vec!["/bin/pwd".into(), "-P".into()]).unwrap();
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir("/")
                .output()
                .unwrap()
        };
        let here = std::env::temp_dir().canonicalize().unwrap();
        let out = run(&here);
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            here.to_str().unwrap()
        );

        let out = run(&here.join("arakun-pty-no-such-folder"));
        assert_eq!(out.status.code(), Some(126));
        assert!(out.stdout.is_empty());
    }
}
