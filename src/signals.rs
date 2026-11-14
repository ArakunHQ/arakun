//! SIGTERM, SIGINT and SIGHUP (Ctrl+C and Ctrl+Break in a Windows console)
//! quit the way closing the window does, so the state, drafts and transcripts
//! are saved first. A second signal is not caught: it ends arakun right away if
//! quitting hangs.

use gpui::App;

pub fn quit_on_signals(cx: &mut App) {
    let Some(rx) = imp::listen() else { return };
    cx.spawn(async move |cx| {
        if rx.await.is_ok() {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

/// Resolves on the first of those signals, for a process without a window.
pub fn first_signal() -> Option<futures::channel::oneshot::Receiver<()>> {
    imp::listen()
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicI32, Ordering};

    use futures::channel::oneshot;

    const SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

    /// The write end of the pipe the handler wakes the listener through.
    static WAKE: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn on_signal(_: libc::c_int) {
        let fd = WAKE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write is async-signal-safe.
            unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
        }
    }

    pub fn listen() -> Option<oneshot::Receiver<()>> {
        let mut fds = [0; 2];
        // SAFETY: plain syscalls on a local array; the handler only writes to the pipe.
        unsafe {
            // macOS has no pipe2: set close-on-exec right after; nothing spawns this early.
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return None;
            }
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            WAKE.store(fds[1], Ordering::Relaxed);
            for sig in SIGNALS {
                libc::signal(sig, on_signal as *const () as libc::sighandler_t);
            }
        }
        let (tx, rx) = oneshot::channel::<()>();
        let _ = std::thread::Builder::new()
            .name("arakun-signals".into())
            .spawn(move || {
                let mut byte = 0u8;
                // SAFETY: reads into a local byte; restoring the default handlers is plain libc.
                unsafe {
                    while libc::read(fds[0], (&mut byte as *mut u8).cast(), 1) < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                    }
                    for sig in SIGNALS {
                        libc::signal(sig, libc::SIG_DFL);
                    }
                }
                let _ = tx.send(());
            });
        Some(rx)
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::Mutex;

    use futures::channel::oneshot;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    static TX: Mutex<Option<oneshot::Sender<()>>> = Mutex::new(None);

    /// Runs on a thread Windows starts for the event. Once the sender is taken, the
    /// next Ctrl+C falls through to the default handler, which ends arakun.
    unsafe extern "system" fn on_ctrl(kind: u32) -> BOOL {
        if kind != CTRL_C_EVENT && kind != CTRL_BREAK_EVENT {
            return 0;
        }
        match TX.lock().ok().and_then(|mut tx| tx.take()) {
            Some(tx) => {
                let _ = tx.send(());
                1
            }
            None => 0,
        }
    }

    pub fn listen() -> Option<oneshot::Receiver<()>> {
        let (tx, rx) = oneshot::channel::<()>();
        *TX.lock().ok()? = Some(tx);
        // SAFETY: registers a handler that only touches the mutex above.
        (unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) } != 0).then_some(rx)
    }
}
