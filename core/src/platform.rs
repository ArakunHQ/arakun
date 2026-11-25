//! Everything that differs between Linux and Windows, so the rest of arakun reads the same on
//! both. The Linux side is what arakun always did; the Windows side keeps the same promises
//! with Windows' own tools.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Where applications keep their data: `$XDG_DATA_HOME` or `~/.local/share` on Linux,
/// `%LOCALAPPDATA%` on Windows.
pub fn data_home() -> PathBuf {
    #[cfg(windows)]
    let (var, fallback) = ("LOCALAPPDATA", home().join("AppData").join("Local"));
    #[cfg(not(windows))]
    let (var, fallback) = ("XDG_DATA_HOME", home().join(".local/share"));
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or(fallback)
}

/// arakun's own data directory.
pub fn app_dir() -> PathBuf {
    data_home().join("arakun")
}

/// `fs::canonicalize` without the `\\?\` prefix Windows adds, so a canonical path still
/// compares equal to, and prints like, the paths the rest of arakun holds.
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// A command that opens no window of its own.
///
/// arakun is a window, not a console program: on Windows every console child it starts
/// (git, curl, an agent) would otherwise flash a console window on screen.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    no_window(&mut cmd);
    cmd
}

/// [`command`] for an agent, or for what an agent runs. On Linux under systemd it starts
/// inside `arakun-agents.slice`, which every agent shares and which may hold at most 70% of
/// RAM: past that the kernel kills the biggest process in there (usually a compiler) instead
/// of the whole desktop running out. Ten nvcc's from one agent once filled 47 GB, and earlyoom
/// took the keyring, and with it every browser login, down with them.
pub fn agent_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(target_os = "linux")]
    if let Some(systemd_run) = agent_slice() {
        let mut cmd = Command::new(systemd_run);
        // --scope execs `program` in place: same pid, process group, cwd and environment.
        cmd.args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--slice=arakun-agents.slice",
            "--",
        ])
        .arg(program);
        return cmd;
    }
    command(program)
}

/// systemd-run, once the agents' slice has its memory cap; None without a systemd user session.
#[cfg(target_os = "linux")]
fn agent_slice() -> Option<&'static Path> {
    static SYSTEMD_RUN: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SYSTEMD_RUN
        .get_or_init(|| {
            let path = std::env::var_os("PATH")?;
            let systemd_run = std::env::split_paths(&path)
                .map(|dir| dir.join("systemd-run"))
                .find(|p| p.is_file())?;
            let capped = Command::new("systemctl")
                .args([
                    "--user",
                    "set-property",
                    "--runtime",
                    "arakun-agents.slice",
                    "MemoryMax=70%",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            capped.then_some(systemd_run)
        })
        .as_deref()
}

pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    cmd
}

/// Lowers a child's priority (and, on Unix, of everything it starts), so builds and tests
/// an agent runs leave the window responsive. Windows children inherit
/// BELOW_NORMAL_PRIORITY_CLASS the same way.
pub fn lower_priority(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW,
        };
        // creation_flags replaces the flags, so CREATE_NO_WINDOW from `no_window` stays.
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: nice is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::nice(10);
                Ok(())
            });
        }
    }
}
/// `script` run by the system shell, without a terminal: `sh -c` on Unix, `cmd /C` on
/// Windows, whose `&&` and `2>&1` work as in sh (Windows PowerShell 5.1 has no `&&`).
pub fn shell_command(script: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let mut cmd = Command::new("cmd.exe");
        // /S: what follows /C is the outer quotes' content, taken as typed.
        cmd.args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{script}\""));
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }
}

/// What `arakun run` and a project's scripts run in a terminal: `command` at lower
/// priority, then `[exit N]`, then an interactive shell (on Unix after Enter), so the
/// output stays readable even when the command calls `exit` or is stopped with Ctrl+C.
pub fn run_then_shell(command: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        // Children inherit the priority class; the shell gets its own back afterwards.
        let script = format!(
            "$arakunMe = Get-Process -Id $PID; $arakunMe.PriorityClass = 'BelowNormal'; $global:LASTEXITCODE = 0\n\
             try {{\n{command}\n}} finally {{\n\
             $arakunCode = if ($?) {{ [int]$LASTEXITCODE }} elseif ($LASTEXITCODE) {{ $LASTEXITCODE }} else {{ 1 }}\n\
             $arakunMe.PriorityClass = 'Normal'\n\
             Write-Host (\"`n\" + [char]27 + \"[2m[exit $arakunCode]\" + [char]27 + \"[0m\")\n}}"
        );
        vec![
            windows_shell(),
            "-NoLogo".into(),
            "-NoExit".into(),
            "-Command".into(),
            script,
        ]
    }
    #[cfg(not(windows))]
    {
        // The shell waits for Enter: a login shell's banner (fastfetch and the
        // like) would otherwise push the output off screen as soon as it ends.
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "trap : INT\nnice -n 10 sh -c \"$1\"\ncode=$?\nprintf '\\n\\033[2m[exit %d]\\033[0m\\n\\033[2m%s\\033[0m' \"$code\" \"$2\"\nread _ || true\nexec \"${SHELL:-/bin/sh}\" -l"
                .to_string(),
            "sh".to_string(),
            command.to_string(),
            RUN_SHELL_HINT.to_string(),
        ]
    }
}

/// The last line of a finished `run_then_shell` terminal until Enter opens its shell.
pub const RUN_SHELL_HINT: &str = "Press Enter for a shell";

/// Whether process `pid` is still running.
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: the handle is checked before use and closed after.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(process, &mut code) != 0;
            CloseHandle(process);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(windows))]
    // SAFETY: signal 0 only checks that the process exists.
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
}

/// Asks process `pid` to end: SIGTERM on Unix; Windows has no such request, so it is ended.
pub fn terminate(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    hang_up(pid);
    #[cfg(not(windows))]
    // SAFETY: plain signal delivery.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// Starts `cmd` apart from arakun's session (Unix) or console (Windows), so it outlives the
/// command and the shell that ran it.
pub fn detach(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd
}
/// Starts this binary again once this process is gone, so it opens a fresh
/// install and finds the control socket free. The running binary may have been
/// replaced on disk since it started; its path still names the new one.
///
/// On macOS Arakun.app is opened through LaunchServices rather than by running its
/// binary. A bundle's binary run directly gets no app identity, and right after an
/// update it can die on its first start while the system checks the new bundle.
#[cfg(unix)]
pub fn relaunch() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy();
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
    let wait = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.1; done; "#;
    let mut cmd = Command::new("sh");
    #[cfg(target_os = "macos")]
    let bundle = canonicalize(exe).ok().and_then(|exe| app_bundle(&exe));
    #[cfg(not(target_os = "macos"))]
    let bundle: Option<PathBuf> = None;
    match bundle {
        Some(app) => cmd
            .args(["-c", &format!(r#"{wait}exec /usr/bin/open -n "$2""#), "sh"])
            .arg(std::process::id().to_string())
            .arg(app),
        None => cmd
            .args(["-c", &format!(r#"{wait}exec "$2""#), "sh"])
            .arg(std::process::id().to_string())
            .arg(exe),
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd).spawn().map(|_| ())
}

/// The `.app` folder holding `exe` when it is a bundle's main binary
/// (`X.app/Contents/MacOS/x`), `None` for a bare binary such as `cargo run`'s.
#[cfg(any(target_os = "macos", test))]
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    is_bundled(exe)
        .then(|| exe.ancestors().nth(3).map(Path::to_path_buf))
        .flatten()
}

#[cfg(not(unix))]
pub fn relaunch() -> std::io::Result<()> {
    Err(std::io::Error::other("not supported on this platform"))
}

/// Gives the Dock the raccoon when arakun runs as a bare binary (`cargo run`,
/// `target/debug/arakun`). Such a process has no bundle and so no icon: macOS shows the
/// generic executable. Inside Arakun.app the bundle's icon is left alone, so the system
/// can still style it.
#[cfg(target_os = "macos")]
pub fn set_dock_icon(png: &[u8]) {
    use std::ffi::{c_char, c_void};

    let bundled = std::env::current_exe().is_ok_and(|exe| {
        exe.parent()
            .is_some_and(|dir| dir.ends_with("Contents/MacOS"))
    });
    if bundled || png.is_empty() {
        return;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    type Id = *mut c_void;
    // SAFETY: Objective-C runtime calls on the main thread (gpui's launch callback), each
    // objc_msgSend cast to the signature of the method it sends. The alloc'ed objects are
    // released here; setApplicationIconImage: keeps its own reference.
    unsafe {
        let send0: extern "C" fn(Id, Id) -> Id = std::mem::transmute(objc_msgSend as *const c_void);
        let send1: extern "C" fn(Id, Id, Id) -> Id =
            std::mem::transmute(objc_msgSend as *const c_void);
        let send_bytes: extern "C" fn(Id, Id, *const u8, usize) -> Id =
            std::mem::transmute(objc_msgSend as *const c_void);
        let sel = |name: &std::ffi::CStr| sel_registerName(name.as_ptr());

        let data = send0(objc_getClass(c"NSData".as_ptr()), sel(c"alloc"));
        let data = send_bytes(data, sel(c"initWithBytes:length:"), png.as_ptr(), png.len());
        if data.is_null() {
            return;
        }
        let image = send0(objc_getClass(c"NSImage".as_ptr()), sel(c"alloc"));
        let image = send1(image, sel(c"initWithData:"), data);
        send0(data, sel(c"release"));
        if image.is_null() {
            return;
        }
        let app = send0(
            objc_getClass(c"NSApplication".as_ptr()),
            sel(c"sharedApplication"),
        );
        if !app.is_null() {
            send1(app, sel(c"setApplicationIconImage:"), image);
        }
        send0(image, sel(c"release"));
    }
}

/// Opens a file, folder or URL with the desktop's default application.
pub fn open(target: impl AsRef<OsStr>) {
    #[cfg(windows)]
    let mut cmd = command("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        // Reaped off the main thread so no zombie is left behind.
        std::thread::spawn(move || child.wait());
    }
}

/// Whether this arakun runs from inside an `.app`. Only then can it post macOS
/// notifications as Arakun.
#[cfg(target_os = "macos")]
pub fn in_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_bundled(&exe))
}

/// `…/Name.app/Contents/MacOS/binary`.
#[cfg(any(test, target_os = "macos"))]
fn is_bundled(exe: &Path) -> bool {
    let mut up = exe.ancestors().skip(1);
    up.next()
        .is_some_and(|d| d.file_name() == Some(OsStr::new("MacOS")))
        && up
            .next()
            .is_some_and(|d| d.file_name() == Some(OsStr::new("Contents")))
        && up
            .next()
            .is_some_and(|d| d.extension() == Some(OsStr::new("app")))
}

/// Puts up a desktop notification. Linux uses `notify-send`; Windows a toast from Windows
/// PowerShell, which ships with every Windows 10/11 and is registered to show toasts;
/// macOS AppleScript's `display notification`. On macOS that notification belongs to
/// Script Editor, so a click opens it: Arakun.app posts its own through gpui instead
/// (`shell::sessions::send_notification`), and this is for a build run outside one.
pub fn notify(title: &str, body: &str, critical: bool, icon: Option<&str>) {
    if let Some(mut child) = spawn_notification(title, body, critical, icon) {
        // Reaped off the main thread so no zombie is left behind.
        std::thread::spawn(move || child.wait());
    }
}

/// [`notify`] for a process about to exit: waits up to `limit` for the notification to
/// be handed to the desktop, so the exit can't cut it off.
pub fn notify_and_wait(title: &str, body: &str, limit: std::time::Duration) {
    let Some(mut child) = spawn_notification(title, body, true, None) else {
        return;
    };
    let until = std::time::Instant::now() + limit;
    while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn spawn_notification(
    title: &str,
    body: &str,
    critical: bool,
    icon: Option<&str>,
) -> Option<std::process::Child> {
    // The test arakun (`arakun dev`) shares the user's desktop session: its
    // notifications would land on the real desktop and pile up there.
    if std::env::var_os("ARAKUN_DEV").is_some() {
        eprintln!("arakun: notification (not shown in the test arakun): {title} · {body}");
        return None;
    }
    #[cfg(windows)]
    let mut cmd = {
        let _ = (critical, icon);
        let mut cmd = command("powershell.exe");
        // Title and body travel in the environment, never inside the script, so nothing
        // in them is ever parsed as PowerShell.
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", TOAST])
            .env("ARAKUN_TOAST_TITLE", title)
            .env("ARAKUN_TOAST_BODY", body);
        cmd
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let _ = (critical, icon);
        // Title and body travel as arguments, never inside the script.
        let mut cmd = Command::new("osascript");
        cmd.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
        ])
        .arg(title)
        .arg(body);
        cmd
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut cmd = Command::new("notify-send");
        cmd.args(["-a", "Arakun"]);
        if let Some(icon) = icon {
            cmd.args(["-i", icon]);
        }
        if critical {
            cmd.args(["-u", "critical"]);
        }
        cmd.arg("--").arg(title).arg(body);
        cmd
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn()
        .inspect_err(|e| eprintln!("arakun: notification: {e}"))
        .ok()
}

/// Starts playing a WAV file; the child ends when the sound does, and killing it stops
/// the sound. Linux tries PipeWire, then PulseAudio, then ALSA; Windows plays it in
/// Windows PowerShell with the SoundPlayer every Windows has; macOS uses `afplay`.
pub fn play_sound(path: &Path) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        // The path travels in the environment so nothing in it is parsed as PowerShell.
        command("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(New-Object System.Media.SoundPlayer $env:ARAKUN_SOUND).PlaySync()",
            ])
            .env("ARAKUN_SOUND", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
    #[cfg(not(windows))]
    {
        let players: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
            &[("afplay", &[])]
        } else {
            &[("pw-play", &[]), ("paplay", &[]), ("aplay", &["-q"])]
        };
        for &(program, args) in players {
            let started = Command::new(program)
                .args(args)
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match started {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                other => return other,
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no pw-play, paplay or aplay to play sound with",
        ))
    }
}

/// Puts an image file on the system clipboard as an image, so other apps can paste it.
/// GPUI's Linux clipboard only offers text, so this goes through wl-copy (Wayland) or
/// xclip (X11); Windows uses the Forms clipboard and macOS AppleScript. Returns once
/// the helper has the data; wl-copy and xclip keep serving it in the background.
pub fn copy_image(path: &Path) -> std::io::Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    };
    let check = |status: std::process::ExitStatus, program: &str| {
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "{program} failed ({status})"
            )))
        }
    };
    #[cfg(windows)]
    {
        let _ = mime;
        let status = command("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-STA",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
                 [System.Windows.Forms.Clipboard]::SetImage([System.Drawing.Image]::FromFile($env:ARAKUN_IMAGE))",
            ])
            .env("ARAKUN_IMAGE", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        check(status, "powershell")
    }
    #[cfg(target_os = "macos")]
    {
        let _ = mime;
        let class = if ext == "jpg" || ext == "jpeg" {
            "JPEG picture"
        } else {
            "«class PNGf»"
        };
        let script = format!(
            "set the clipboard to (read (POSIX file (system attribute \"ARAKUN_IMAGE\")) as {class})"
        );
        let status = Command::new("osascript")
            .args(["-e", &script])
            .env("ARAKUN_IMAGE", path)
            .status()?;
        check(status, "osascript")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let file = std::fs::File::open(path)?;
        let (program, args): (&str, Vec<&str>) = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            ("wl-copy", vec!["--type", mime])
        } else {
            ("xclip", vec!["-selection", "clipboard", "-t", mime, "-i"])
        };
        let status = Command::new(program)
            .args(args)
            .stdin(file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        check(status, program)
    }
}
#[cfg(windows)]
const TOAST: &str = "\
$ErrorActionPreference = 'Stop'
$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$text = $xml.GetElementsByTagName('text')
$null = $text.Item(0).AppendChild($xml.CreateTextNode($env:ARAKUN_TOAST_TITLE))
$null = $text.Item(1).AppendChild($xml.CreateTextNode($env:ARAKUN_TOAST_BODY))
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($toast)
";

/// The program and arguments a new terminal runs when nothing else is asked for.
pub fn default_shell() -> Vec<String> {
    #[cfg(windows)]
    return vec![windows_shell(), "-NoLogo".into()];
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        vec![shell, "-l".into()]
    }
}

/// PowerShell 7 when it is installed, else the Windows PowerShell every Windows has.
#[cfg(windows)]
fn windows_shell() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    match find_in(std::env::split_paths(&path), "pwsh") {
        Some(_) => "pwsh.exe".into(),
        None => "powershell.exe".into(),
    }
}

/// Whether `path` is something a `Command` can start: a file with an execute bit on Unix,
/// a file with one of `%PATHEXT%`'s extensions on Windows.
pub fn is_executable(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()));
        path.is_file()
            && ext.is_some_and(|ext| {
                path_exts()
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&ext))
            })
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
}

/// The first executable called `name` in `dirs`. On Windows `name` may leave out its
/// extension (`npm` finds `npm.cmd`), as a shell would.
pub fn find_in(dirs: impl IntoIterator<Item = PathBuf>, name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let path = dir.join(name);
        if is_executable(&path) {
            return Some(path);
        }
        #[cfg(windows)]
        for ext in path_exts() {
            let mut with_ext = path.clone().into_os_string();
            with_ext.push(&ext);
            let with_ext = PathBuf::from(with_ext);
            if is_executable(&with_ext) {
                return Some(with_ext);
            }
        }
    }
    None
}

#[cfg(windows)]
fn path_exts() -> Vec<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| e.starts_with('.'))
        .map(str::to_string)
        .collect()
}

/// Whether `name` names a path rather than a program to look up.
pub fn is_path(name: &str) -> bool {
    name.contains('/') || (cfg!(windows) && name.contains('\\'))
}

/// 16 random bytes from the operating system.
pub fn random_bytes() -> Option<[u8; 16]> {
    let mut b = [0u8; 16];
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        // SAFETY: the buffer is ours and its length is the one passed.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                b.as_mut_ptr(),
                b.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        (status == 0).then_some(b)
    }
    #[cfg(not(windows))]
    {
        // SAFETY: getentropy fills exactly `b.len()` (at most 256) bytes of the buffer we own.
        let rc = unsafe { libc::getentropy(b.as_mut_ptr().cast(), b.len()) };
        (rc == 0).then_some(b)
    }
}

/// Middle-click paste reads the primary selection, which only Linux has.
pub fn read_primary(cx: &gpui::App) -> Option<gpui::ClipboardItem> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    return cx.read_from_primary();
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = cx;
        None
    }
}

pub fn write_primary(cx: &mut gpui::App, item: gpui::ClipboardItem) {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    cx.write_to_primary(item);
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = (cx, item);
}

/// A shortcut as a `&'static str`, which gpui's `KeyBinding::new` takes. Each
/// distinct text is leaked once and reused, so binding the same keys again
/// costs nothing new.
pub fn static_keys(keys: &str) -> &'static str {
    static KEYS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut known = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = known.iter().find(|k| **k == keys) {
        return k;
    }
    let k: &'static str = Box::leak(keys.to_owned().into_boxed_str());
    known.push(k);
    k
}

/// Shortcuts are written the Windows and Linux way ("ctrl-k"). macOS takes Cmd
/// where those take Ctrl, but leaves Ctrl on the keys whose Cmd chord the system
/// owns (Cmd+Tab switches apps, Cmd+` windows), and goes back and forward with
/// Cmd+[ and Cmd+], since Option+arrows move by word there.
pub fn keystroke(keys: &str) -> String {
    if !cfg!(target_os = "macos") {
        return keys.to_string();
    }
    match keys {
        "alt-left" => return "cmd-[".into(),
        "alt-right" => return "cmd-]".into(),
        _ => {}
    }
    let key = keys.rsplit('-').next().unwrap_or(keys);
    let key = if key.is_empty() { "-" } else { key };
    match keys.strip_prefix("ctrl-") {
        Some(rest) if !matches!(key, "tab" | "`" | "pageup" | "pagedown") => format!("cmd-{rest}"),
        _ => keys.to_string(),
    }
}

/// A shortcut as the UI shows it, from the same Windows and Linux spelling
/// ("Ctrl+Shift+T"): on macOS the symbols, in Apple's order ("⇧⌘T"), with the
/// same exceptions as `keystroke`. Text around the shortcuts is kept.
pub fn shortcut_label(text: &str) -> String {
    if !cfg!(target_os = "macos") {
        return text.to_string();
    }
    let text = text.replace("Alt+Left", "Cmd+[").replace("Alt+←", "Cmd+[");
    let text = text.replace("Alt+Right", "Cmd+]").replace("Alt+→", "Cmd+]");
    let mut out = String::new();
    let mut rest = text.as_str();
    const MODS: [&str; 4] = ["Ctrl+", "Alt+", "Shift+", "Cmd+"];
    while let Some(at) = MODS.iter().filter_map(|m| rest.find(m)).min() {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (mut ctrl, mut alt, mut shift, mut cmd) = (false, false, false, false);
        while let Some(m) = MODS.iter().find(|m| rest.starts_with(**m)) {
            match *m {
                "Ctrl+" => ctrl = true,
                "Alt+" => alt = true,
                "Shift+" => shift = true,
                _ => cmd = true,
            }
            rest = &rest[m.len()..];
        }
        let n = match rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .count()
        {
            0 => rest.chars().next().map_or(0, char::len_utf8),
            n => n,
        };
        let key = &rest[..n];
        rest = &rest[n..];
        let keep_ctrl = matches!(key, "Tab" | "`" | "PgDn" | "PgUp");
        for (on, sym) in [
            (ctrl && keep_ctrl, "⌃"),
            (alt, "⌥"),
            (shift, "⇧"),
            (cmd || ctrl && !keep_ctrl, "⌘"),
        ] {
            if on {
                out.push_str(sym);
            }
        }
        out.push_str(match key {
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            "Enter" => "↩",
            k => k,
        });
    }
    out.push_str(rest);
    out
}

/// Key caps given the Windows and Linux way (`["Ctrl", "⇧", "N"]`), as this
/// platform shows them: on macOS the symbols in Apple's order (`["⇧", "⌘", "N"]`),
/// with Ctrl kept for the same keys as `keystroke`.
pub fn keycaps(keys: &[&str]) -> Vec<String> {
    if cfg!(target_os = "macos") {
        mac_keycaps(keys)
    } else {
        keys.iter().map(|k| k.to_string()).collect()
    }
}

fn mac_keycaps(keys: &[&str]) -> Vec<String> {
    let keep_ctrl = keys
        .last()
        .is_some_and(|k| matches!(*k, "Tab" | "`" | "PgUp" | "PgDn"));
    let mut mods = [false; 4];
    let mut rest = Vec::new();
    for k in keys {
        match *k {
            "Ctrl" if keep_ctrl => mods[0] = true,
            "Alt" | "⌥" => mods[1] = true,
            "Shift" | "⇧" => mods[2] = true,
            "Ctrl" | "Cmd" | "⌘" => mods[3] = true,
            k => rest.push(k.to_string()),
        }
    }
    ["⌃", "⌥", "⇧", "⌘"]
        .iter()
        .zip(mods)
        .filter(|(_, on)| *on)
        .map(|(sym, _)| sym.to_string())
        .chain(rest)
        .collect()
}

/// Whether the platform's command key is the only modifier held besides
/// Shift: Cmd on macOS, Ctrl elsewhere.
pub fn command_key(m: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform && !m.control && !m.alt
    } else {
        m.control && !m.platform && !m.alt
    }
}

/// The modifier that moves the caret a word at a time: Option on macOS, Ctrl elsewhere.
pub fn word_key(m: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.alt && !m.platform
    } else {
        m.control
    }
}

/// On Windows arakun is a console program, so `arakun <command>` prints in the terminal that
/// ran it. Started any other way (the Start menu, Explorer) Windows gives it a console
/// window of its own; that one is let go, and what arakun prints goes to its log instead.
pub fn detach_own_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            FreeConsole, GetConsoleProcessList, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        };
        let mut ids = [0u32; 2];
        // SAFETY: the buffer holds `ids.len()` process ids.
        let attached = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
        if attached != 1 {
            return;
        }
        // SAFETY: plain calls without pointers; the log handle is leaked on purpose, it
        // stays the process's stdout/stderr until it exits.
        unsafe {
            FreeConsole();
            if let Some(log) = open_log() {
                use std::os::windows::io::IntoRawHandle as _;
                let handle = log.into_raw_handle();
                SetStdHandle(STD_OUTPUT_HANDLE, handle);
                SetStdHandle(STD_ERROR_HANDLE, handle);
            }
        }
    }
}

#[cfg(target_os = "macos")]
static LOGIN_PATH: std::sync::OnceLock<std::ffi::OsString> = std::sync::OnceLock::new();

/// Refreshed login-shell `PATH` discovered in the background on macOS after
/// launching from Finder/Dock, if available.
pub fn login_path_override() -> Option<&'static std::ffi::OsStr> {
    #[cfg(target_os = "macos")]
    {
        LOGIN_PATH.get().map(std::ffi::OsString::as_os_str)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_login_path(stdout: &str) -> Option<String> {
    stdout
        .trim_end_matches(['\r', '\n'])
        .rsplit('\n')
        .next()
        .map(str::trim)
        .filter(|p| p.contains('/'))
        .map(str::to_string)
}

#[cfg(any(target_os = "macos", test))]
fn fallback_macos_path(current: Option<&std::ffi::OsStr>, home: &Path) -> std::ffi::OsString {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
    ];
    if let Some(cur) = current {
        for d in std::env::split_paths(cur) {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn login_path_cache_file() -> PathBuf {
    app_dir().join("login-path.txt")
}

#[cfg(target_os = "macos")]
fn read_login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", r#"printf '\n%s' "$PATH""#]);
    let out = stdout_within(cmd, std::time::Duration::from_secs(5))?;
    // What an interactive rc prints comes before the newline the command starts with.
    parse_login_path(&out)
}

/// Runs `cmd` and returns its stdout, or `None` if it does not finish within `limit`.
/// Stdout is drained on its own thread while the command runs: a command that prints
/// more than the pipe holds (about 64 KB) would otherwise block until it is killed.
#[cfg(any(target_os = "macos", all(unix, test)))]
fn stdout_within(mut cmd: Command, limit: std::time::Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Not joined: a daemon the rc file starts can keep the pipe open past the deadline.
    std::thread::Builder::new()
        .name("arakun-login-path-read".into())
        .spawn(move || {
            let mut out = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
            let _ = tx.send(out);
        })
        .ok()?;
    let out = rx.recv_timeout(limit);
    if out.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    Some(String::from_utf8_lossy(&out.ok()?).into_owned())
}

/// Started from Finder or the Dock, macOS gives an app launchd's `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`), where no agent, Homebrew or `~/.local/bin` is.
/// Applies the cached login-shell `PATH` (or standard macOS tool directories on
/// first launch) synchronously with zero subprocess startup cost. Call before
/// any thread starts, then call [`refresh_login_path_async`] once env setup is done.
pub fn import_login_path() {
    #[cfg(target_os = "macos")]
    {
        if std::os::unix::process::parent_id() != 1 {
            return;
        }
        let cached = std::fs::read_to_string(login_path_cache_file())
            .ok()
            .and_then(|s| parse_login_path(&s));
        let path = match cached {
            Some(p) => std::ffi::OsString::from(p),
            None => fallback_macos_path(std::env::var_os("PATH").as_deref(), &home()),
        };
        // SAFETY: called first thing in `main`, before the app starts any thread.
        unsafe { std::env::set_var("PATH", path) };
    }
}

#[cfg(target_os = "macos")]
fn write_login_path_cache(file: &Path, path: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("txt.tmp");
    std::fs::write(&tmp, path)?;
    std::fs::rename(&tmp, file)
}

/// Refreshes the user's login-shell `PATH` off the startup critical path on
/// macOS so subsequent child spawns and launches see any `.zshrc` updates
/// without blocking window creation.
pub fn refresh_login_path_async() {
    #[cfg(target_os = "macos")]
    {
        if std::os::unix::process::parent_id() != 1 {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("arakun-login-path".into())
            .spawn(|| {
                let Some(path) = read_login_shell_path() else {
                    return;
                };
                let _ = LOGIN_PATH.set(std::ffi::OsString::from(&path));
                let _ = write_login_path_cache(&login_path_cache_file(), &path);
            });
    }
}

/// `arakun.log` in arakun's data folder, started over past 5 MB.
#[cfg(windows)]
fn open_log() -> Option<std::fs::File> {
    let dir = app_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("arakun.log");
    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 << 20);
    std::fs::OpenOptions::new()
        .create(true)
        .append(!too_big)
        .write(true)
        .truncate(too_big)
        .open(path)
        .ok()
}

/// Ends a terminal's shell the way closing its window would: SIGHUP on Unix; on Windows,
/// where a shell has no hangup, the process is ended.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hang_up(pid: u32) {
    if pid == 0 {
        // Never spawned; on Unix 0 would signal arakun's own process group.
        return;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };
        // SAFETY: the handle is checked before use and closed after.
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
    }
    #[cfg(not(windows))]
    // SAFETY: plain signal delivery.
    unsafe {
        libc::kill(pid as i32, libc::SIGHUP);
    }
}

/// An agent and everything it starts, ended together.
///
/// Claude's adapter runs the CLI under node, so ending only the process arakun started
/// would leave the rest running. Unix puts the agent in its own process group and asks
/// the kernel to end it if arakun dies. Windows has neither: a job object holds the tree,
/// and closing arakun's handle to it (on purpose or because arakun died) ends everything in it.
pub struct ProcessTree {
    #[cfg(not(windows))]
    group: i32,
    /// The job's HANDLE, as an integer so the tree can move to the reaper thread.
    #[cfg(windows)]
    job: isize,
}

impl ProcessTree {
    /// Readies `cmd` before it is spawned.
    pub fn prepare(cmd: &mut Command) {
        no_window(cmd);
        Self::prepare_made(cmd);
    }

    /// [`prepare`](Self::prepare) for a command made with [`command`] or [`agent_command`],
    /// which already opens no window. Windows has one set of creation flags, and setting
    /// them again would undo what [`lower_priority`] set.
    pub fn prepare_made(cmd: &mut Command) {
        #[cfg(not(windows))]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
            // macOS has no parent-death signal; the agent outlives a crashed arakun there.
            #[cfg(target_os = "linux")]
            // SAFETY: prctl is async-signal-safe; this only asks the kernel to SIGTERM the
            // agent if arakun dies without running destructors.
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        let _ = cmd;
    }

    /// [`prepare_made`](Self::prepare_made) for a command with a short time limit
    /// (git and gh polls): its own process group, so the limit can stop the whole tree, but
    /// no parent-death signal. That signal needs `pre_exec`, and a command with
    /// `pre_exec` is started with a full `fork()` of arakun instead of
    /// `posix_spawn`. The fork holds the process's memory map while it copies a
    /// large address space, and the window stalls on its next page fault: up to
    /// seconds, with git and gh running every few seconds. A short command ends by
    /// itself or at its limit, so outliving a crashed arakun briefly is harmless.
    pub fn prepare_short(cmd: &mut Command) {
        #[cfg(not(windows))]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        let _ = cmd;
    }

    /// The tree rooted at `child`, which was spawned from a [`prepare`](Self::prepare)d command.
    pub fn adopt(child: &std::process::Child) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };
            // SAFETY: the job handle is checked, the info struct outlives the call, and the
            // child's handle is valid for as long as `child` is.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if !job.is_null() {
                    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        (&raw const info).cast(),
                        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    );
                    AssignProcessToJobObject(job, child.as_raw_handle());
                }
                Self { job: job as isize }
            }
        }
        #[cfg(not(windows))]
        Self {
            group: child.id() as i32,
        }
    }

    /// Asks the tree to end, giving it a chance to flush. Windows has no such request; there
    /// the agent sees its stdin close when the connection goes, which is the same ask.
    pub fn terminate(&self) {
        #[cfg(not(windows))]
        // SAFETY: plain signal delivery to the process group we created.
        unsafe {
            libc::kill(-self.group, libc::SIGTERM);
        }
    }

    /// Whether anything in the tree still runs, a zombie the caller has not reaped
    /// included.
    #[cfg(not(windows))]
    fn is_running(&self) -> bool {
        // SAFETY: signal 0 only checks that the group exists.
        unsafe { libc::kill(-self.group, 0) == 0 }
    }

    /// Lets what is left in the tree keep running once this is dropped. For helpers a
    /// command starts on purpose and expects to outlive it, such as the
    /// `fsmonitor--daemon` that `git status` starts.
    pub fn release(self) {
        #[cfg(windows)]
        if self.job != 0 {
            use windows_sys::Win32::System::JobObjects::{
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };
            // SAFETY: the handle is ours until drop, and the info struct outlives the
            // call. No flags clears KILL_ON_JOB_CLOSE, so closing the handle ends nothing.
            unsafe {
                let info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                SetInformationJobObject(
                    self.job as _,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
            }
        }
    }

    /// Ends the whole tree now.
    pub fn kill(&self) {
        #[cfg(windows)]
        if self.job != 0 {
            // SAFETY: the handle is ours until drop.
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job as _, 1);
            }
        }
        #[cfg(not(windows))]
        // SAFETY: as in `terminate`.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.job != 0 {
            // SAFETY: closing the handle we created; with KILL_ON_JOB_CLOSE this also ends
            // whatever is still in the job.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.job as _);
            }
        }
    }
}

/// How long a git call that only reads or writes the repository on disk may run. A status
/// scan of a big monorepo on a cold disk can take tens of seconds; one stuck on a hung
/// network drive must still end.
pub const GIT_LOCAL: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a git or gh call that talks to a remote may run: `git fetch`, `gh api`,
/// `gh pr view`. A slow link gets time; a stalled transfer, or ssh waiting on a host
/// that does not answer, does not hold a thread for good.
pub const GIT_REMOTE: std::time::Duration = std::time::Duration::from_secs(120);
/// How long git work the user started and watches may run when it runs hooks or moves a
/// lot of data: clone, commit, push, pull, merge, adding a worktree. A pre-push hook may
/// run a whole test suite, so this only makes sure a hung one ends.
pub const GIT_LONG: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How long a stopped command has to clean up before it is killed.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// [`Command::output`], `input` on its stdin, but the command and everything it started
/// are stopped once it has run for `limit`. Background work runs on a few shared threads,
/// so one call that hangs (a stalled transfer, a slow disk) must not hold one for good.
///
/// The tree is asked to end first and killed after [`STOP_GRACE`]: git removes its lock
/// files on SIGTERM but not when killed, and a stale `index.lock` would break the next
/// commit. Windows has no such ask, so there the tree is killed at once.
///
/// Make `cmd` with [`command`] or [`agent_command`]; this leaves its window and priority
/// as they are.
/// The longest limit [`output_until`] starts without a fork (see
/// [`ProcessTree::prepare_short`]).
const SHORT_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

pub fn output_within(
    cmd: &mut Command,
    input: Option<&[u8]>,
    limit: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    output_until(
        cmd,
        input,
        limit,
        &std::sync::atomic::AtomicBool::new(false),
    )
}

/// [`output_within`] that also stops the command once `stop` is set, with an
/// `Interrupted` error: for a run whose answer nobody waits for any more.
pub fn output_until(
    cmd: &mut Command,
    input: Option<&[u8]>,
    limit: std::time::Duration,
    stop: &std::sync::atomic::AtomicBool,
) -> std::io::Result<std::process::Output> {
    use std::io::{Read as _, Write as _};
    use std::time::{Duration, Instant};
    // Stopped before it starts: no process, so no prompt sent and nothing spent.
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            format!("{} was stopped", command_name(cmd)),
        ));
    }
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    // A short limit (git and gh polls) skips the fork that the parent-death signal
    // costs; a long one (a push, a clone, a host command) keeps the signal, so a
    // hung command doesn't outlive a crashed arakun. Those start rarely.
    if limit <= SHORT_LIMIT {
        ProcessTree::prepare_short(cmd);
    } else {
        ProcessTree::prepare_made(cmd);
    }
    let mut child = cmd.spawn()?;
    let tree = ProcessTree::adopt(&child);
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        let bytes = bytes.to_vec();
        // A thread, so a command that answers before it reads all its input can't block us.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let (tx, rx) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();
    let pipes: [(bool, Option<Box<dyn std::io::Read + Send>>); 2] = [
        (true, child.stdout.take().map(|p| Box::new(p) as _)),
        (false, child.stderr.take().map(|p| Box::new(p) as _)),
    ];
    for (is_stdout, pipe) in pipes {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send((is_stdout, buf));
        });
    }
    drop(tx);
    let started = Instant::now();
    // Most git calls end within a few milliseconds, and a flat 20 ms wait added up to
    // that much to each: look again soon at first, then less often.
    let mut wait = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if stop.load(std::sync::atomic::Ordering::Relaxed) => {
                self::stop(&tree, &mut child);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    format!("{} was stopped", command_name(cmd)),
                ));
            }
            Ok(None) if started.elapsed() < limit => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(20));
            }
            Ok(None) => {
                self::stop(&tree, &mut child);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "{} took longer than {} and was stopped",
                        command_name(cmd),
                        span(limit)
                    ),
                ));
            }
            Err(e) => {
                tree.kill();
                // The tree may not hold the child (on Windows the job object can fail
                // to take it), and the wait would then last as long as the child.
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    };
    let (mut stdout, mut stderr) = (None, None);
    let mut take = |is_stdout: bool, bytes: Vec<u8>| {
        if is_stdout {
            stdout = Some(bytes);
        } else {
            stderr = Some(bytes);
        }
    };
    // The tree is left alone when the pipes close: git starts helpers on purpose that
    // outlive it, such as the fsmonitor daemon, and killing them after every status
    // would make them useless. Something it started that holds a pipe open does get
    // killed, or the call would last as long as that does.
    let until = Instant::now() + STOP_GRACE;
    let mut open = 2;
    while open > 0 {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok((is_stdout, bytes)) => {
                take(is_stdout, bytes);
                open -= 1;
            }
            Err(_) => break,
        }
    }
    if open == 0 {
        tree.release();
    } else {
        tree.kill();
        // What a process outside the tree still holds open is given up on.
        let until = Instant::now() + KILLED_PIPES;
        while let Ok((is_stdout, bytes)) =
            rx.recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            take(is_stdout, bytes);
        }
    }
    Ok(std::process::Output {
        status,
        stdout: stdout.unwrap_or_default(),
        stderr: stderr.unwrap_or_default(),
    })
}

/// How long the pipes get to close once what held them was killed.
const KILLED_PIPES: std::time::Duration = std::time::Duration::from_millis(500);

/// Asks the tree to end, then kills what is left after [`STOP_GRACE`]. Windows has no
/// such ask, so there it is killed at once.
fn stop(tree: &ProcessTree, child: &mut std::process::Child) {
    #[cfg(not(windows))]
    {
        tree.terminate();
        let asked = std::time::Instant::now();
        let mut reaped = false;
        // The whole group gets the grace, not only the child: what it started may
        // still be cleaning up after the child is gone.
        while asked.elapsed() < STOP_GRACE {
            reaped = reaped || matches!(child.try_wait(), Ok(Some(_)));
            if reaped && !tree.is_running() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    tree.kill();
    // The tree may not hold the child (on Windows the job object can fail to take it),
    // and the wait would then last as long as the child.
    let _ = child.kill();
    let _ = child.wait();
}

/// `git fetch` for `git fetch --all`, `gh pr list` for `gh -R o/r pr list`: the program and
/// its subcommand, for an error that says which call hung.
fn command_name(cmd: &Command) -> String {
    let program = Path::new(cmd.get_program()).file_stem().map_or_else(
        || "a command".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let word = |a: &str| {
        !a.starts_with('-')
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let mut words = vec![program];
    let mut args = cmd.get_args().map(|a| a.to_string_lossy());
    while let Some(arg) = args.next() {
        if word(&arg) {
            words.push(arg.into_owned());
            // `gh pr list`: the subcommand is the words that follow, up to the first option.
            words.extend(
                args.by_ref()
                    .map_while(|a| word(&a).then(|| a.into_owned()))
                    .take(1),
            );
            break;
        }
        if matches!(arg.as_ref(), "-c" | "-C" | "-R") {
            // The value of an option before the subcommand.
            args.next();
        }
    }
    words.join(" ")
}

/// `90 s`, or `2 min` for a whole number of minutes.
fn span(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else if secs == 0 {
        format!("{} ms", d.as_millis())
    } else {
        format!("{secs} s")
    }
}

/// Local calendar time, as `struct tm` counts it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalTime {
    pub year: i32,
    /// 0-based, as in `struct tm`.
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    /// 0 is Sunday.
    pub wday: i32,
}

pub fn local_time(unix: u64) -> Option<LocalTime> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{
            FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime,
        };
        let ticks = unix
            .checked_mul(10_000_000)?
            .checked_add(FILETIME_UNIX_EPOCH)?;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        // SAFETY: plain data in and out; a null zone means the current one.
        unsafe {
            let mut utc: SYSTEMTIME = std::mem::zeroed();
            let mut local: SYSTEMTIME = std::mem::zeroed();
            if FileTimeToSystemTime(&ft, &mut utc) == 0
                || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
            {
                return None;
            }
            Some(LocalTime {
                year: local.wYear as i32,
                month: local.wMonth as i32 - 1,
                day: local.wDay as i32,
                hour: local.wHour as i32,
                minute: local.wMinute as i32,
                wday: local.wDayOfWeek as i32,
            })
        }
    }
    #[cfg(not(windows))]
    {
        let t = unix as libc::time_t;
        // SAFETY: `tm` is plain data; localtime_r only writes into it.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return None;
        }
        Some(LocalTime {
            year: tm.tm_year + 1900,
            month: tm.tm_mon,
            day: tm.tm_mday,
            hour: tm.tm_hour,
            minute: tm.tm_min,
            wday: tm.tm_wday,
        })
    }
}

/// Local date and time to unix seconds; out-of-range fields roll over (day 32 is the 1st
/// of the next month), as `mktime` does.
pub fn from_local(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{
            SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime,
        };
        let (year, month, day, hour, minute) = normalize(year, month, day, hour, minute);
        let local = SYSTEMTIME {
            wYear: u16::try_from(year).ok()?,
            wMonth: (month + 1) as u16,
            wDayOfWeek: 0,
            wDay: day as u16,
            wHour: hour as u16,
            wMinute: minute as u16,
            wSecond: 0,
            wMilliseconds: 0,
        };
        // SAFETY: plain data in and out; a null zone means the current one.
        unsafe {
            let mut utc: SYSTEMTIME = std::mem::zeroed();
            let mut ft: FILETIME = std::mem::zeroed();
            if TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) == 0
                || SystemTimeToFileTime(&utc, &mut ft) == 0
            {
                return None;
            }
            let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
            ticks
                .checked_sub(FILETIME_UNIX_EPOCH)
                .map(|t| t / 10_000_000)
        }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: as in `local_time`; mktime normalizes the fields in place.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = year - 1900;
        tm.tm_mon = month;
        tm.tm_mday = day;
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_isdst = -1;
        let t = unsafe { libc::mktime(&mut tm) };
        (t >= 0).then_some(t as u64)
    }
}

/// 100 ns ticks from 1601-01-01, where FILETIME counts from, to 1970-01-01.
#[cfg(windows)]
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// Rolls out-of-range fields over the way `mktime` does: `(2026, 9, 32, 25, 0)` is the
/// 2nd of November at 01:00. Month is 0-based.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn normalize(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> (i32, i32, i32, i32, i32) {
    let minutes = i64::from(hour) * 60 + i64::from(minute);
    let (carry, minute_of_day) = (minutes.div_euclid(1440), minutes.rem_euclid(1440));
    let year = i64::from(year) + i64::from(month).div_euclid(12);
    let month = i64::from(month).rem_euclid(12);
    let days = days_from_civil(year, month + 1, 1) + i64::from(day) - 1 + carry;
    let (y, m, d) = civil_from_days(days);
    (
        y as i32,
        m as i32 - 1,
        d as i32,
        (minute_of_day / 60) as i32,
        (minute_of_day % 60) as i32,
    )
}

/// Days since 1970-01-01 of a proleptic Gregorian date (month 1-based). Howard Hinnant's
/// algorithm, exact for every year an i64 holds.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A secret in the desktop keyring: the Secret Service (through `secret-tool`) on Linux,
/// the Keychain on macOS, the Credential Manager on Windows. `account` tells arakun's
/// secrets apart.
pub mod keyring {
    pub fn lookup(account: &str) -> Option<String> {
        lookup_in("arakun", account)
    }

    /// A secret stored by another app, e.g. droid's "Factory CLI" keyring entries.
    pub fn lookup_in(service: &str, account: &str) -> Option<String> {
        #[cfg(windows)]
        let secret = win::read(&target_name(service, account));
        // Only arakun's own items come from the Keychain: reading another app's item
        // would ask the user to allow it.
        #[cfg(target_os = "macos")]
        let secret = if service == "arakun" {
            // Before the Keychain, a Mac with `secret-tool` kept these there: a
            // secret stored then is still found.
            mac::read(account).or_else(|| secret_tool::lookup(service, account))
        } else {
            secret_tool::lookup(service, account)
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let secret = secret_tool::lookup(service, account);
        secret
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// The name a secret is filed under in the Windows Credential Manager, as it lists
    /// it. Arakun's own are `arakun:<account>`; another app's are `<service>/<account>`,
    /// the form Node's keytar uses (droid's "Factory CLI/auth-encryption-key").
    /// Storing and looking up both go through here, so they can't drift apart.
    #[cfg(any(windows, test))]
    pub(super) fn target_name(service: &str, account: &str) -> String {
        if service == "arakun" {
            format!("arakun:{account}")
        } else {
            format!("{service}/{account}")
        }
    }

    pub fn store(account: &str, label: &str, secret: &str) -> Result<(), String> {
        #[cfg(windows)]
        return win::write(&target_name("arakun", account), label, secret);
        #[cfg(target_os = "macos")]
        return mac::write(account, label, secret);
        #[cfg(not(any(windows, target_os = "macos")))]
        secret_tool::store(account, label, secret)
    }

    pub fn clear(account: &str) {
        #[cfg(windows)]
        win::delete(&target_name("arakun", account));
        #[cfg(target_os = "macos")]
        mac::delete(account);
        // On macOS too: a secret from before the Keychain would come back on
        // the next lookup.
        #[cfg(not(windows))]
        secret_tool::clear(account);
    }

    #[cfg(not(windows))]
    mod secret_tool {
        use std::process::{Command, Stdio};

        pub fn lookup(service: &str, account: &str) -> Option<String> {
            let out = Command::new("secret-tool")
                .args(["lookup", "service", service, "account", account])
                .stderr(Stdio::null())
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8(out.stdout).ok())
                .flatten()
        }

        #[cfg(not(target_os = "macos"))]
        pub fn store(account: &str, label: &str, secret: &str) -> Result<(), String> {
            use std::io::Write as _;
            let mut child = Command::new("secret-tool")
                .args([
                    "store",
                    &format!("--label={label}"),
                    "service",
                    "arakun",
                    "account",
                    account,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("secret-tool: {e}"))?;
            child
                .stdin
                .take()
                .ok_or("secret-tool: no stdin")?
                .write_all(secret.as_bytes())
                .map_err(|e| e.to_string())?;
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
        }

        pub fn clear(account: &str) {
            let _ = Command::new("secret-tool")
                .args(["clear", "service", "arakun", "account", account])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    /// Generic passwords in the default keychain, service "arakun". The Security API,
    /// rather than `security add-generic-password -w <secret>`, keeps the secret out of
    /// the process list.
    #[cfg(target_os = "macos")]
    mod mac {
        use std::ffi::c_void;
        use std::ptr::{null, null_mut};

        const SERVICE: &[u8] = b"arakun";
        /// errSecItemNotFound.
        const ITEM_NOT_FOUND: i32 = -25300;

        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn SecKeychainFindGenericPassword(
                keychain: *const c_void,
                service_len: u32,
                service: *const u8,
                account_len: u32,
                account: *const u8,
                len: *mut u32,
                data: *mut *mut c_void,
                item: *mut *mut c_void,
            ) -> i32;
            fn SecKeychainItemFreeContent(attrs: *const c_void, data: *mut c_void) -> i32;
            fn SecKeychainAddGenericPassword(
                keychain: *const c_void,
                service_len: u32,
                service: *const u8,
                account_len: u32,
                account: *const u8,
                len: u32,
                data: *const u8,
                item: *mut *mut c_void,
            ) -> i32;
            fn SecKeychainItemModifyAttributesAndData(
                item: *mut c_void,
                attrs: *const c_void,
                len: u32,
                data: *const c_void,
            ) -> i32;
            fn SecKeychainItemDelete(item: *mut c_void) -> i32;
        }

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFRelease(cf: *const c_void);
        }

        /// A keychain item reference, released when dropped.
        struct Item(*mut c_void);

        impl Drop for Item {
            fn drop(&mut self) {
                // SAFETY: `find` makes an Item only from a non-null reference the
                // keychain handed over, and nothing else releases it.
                unsafe { CFRelease(self.0) }
            }
        }

        /// The item filed under `account`, or the status the keychain answered.
        fn find(account: &str) -> Result<Option<Item>, i32> {
            let mut item = null_mut();
            // SAFETY: the byte buffers live through the call. Null length and data ask
            // for the item reference only.
            let status = unsafe {
                SecKeychainFindGenericPassword(
                    null(),
                    SERVICE.len() as u32,
                    SERVICE.as_ptr(),
                    account.len() as u32,
                    account.as_ptr(),
                    null_mut(),
                    null_mut(),
                    &mut item,
                )
            };
            match status {
                0 if !item.is_null() => Ok(Some(Item(item))),
                0 | ITEM_NOT_FOUND => Ok(None),
                s => Err(s),
            }
        }

        pub fn read(account: &str) -> Option<String> {
            let (mut len, mut data) = (0u32, null_mut());
            // SAFETY: the buffers live through the call. On success the keychain owns
            // `len` bytes at `data`, which are copied, then released.
            unsafe {
                let status = SecKeychainFindGenericPassword(
                    null(),
                    SERVICE.len() as u32,
                    SERVICE.as_ptr(),
                    account.len() as u32,
                    account.as_ptr(),
                    &mut len,
                    &mut data,
                    null_mut(),
                );
                if status != 0 || data.is_null() {
                    return None;
                }
                let bytes = std::slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec();
                SecKeychainItemFreeContent(null(), data);
                String::from_utf8(bytes).ok()
            }
        }

        /// Keychain Access names a generic password after its service, so `_label`
        /// has nowhere to go.
        pub fn write(account: &str, _label: &str, secret: &str) -> Result<(), String> {
            let status = match find(account) {
                // SAFETY: `item` is a live reference and `secret` outlives the call.
                // Null attributes change the data only.
                Ok(Some(item)) => unsafe {
                    SecKeychainItemModifyAttributesAndData(
                        item.0,
                        null(),
                        secret.len() as u32,
                        secret.as_ptr().cast(),
                    )
                },
                // SAFETY: the byte buffers live through the call. Null selects the
                // default keychain and asks for no item back.
                Ok(None) => unsafe {
                    SecKeychainAddGenericPassword(
                        null(),
                        SERVICE.len() as u32,
                        SERVICE.as_ptr(),
                        account.len() as u32,
                        account.as_ptr(),
                        secret.len() as u32,
                        secret.as_ptr(),
                        null_mut(),
                    )
                },
                Err(status) => status,
            };
            if status == 0 {
                Ok(())
            } else {
                Err(format!("Keychain: error {status}"))
            }
        }

        pub fn delete(account: &str) {
            if let Ok(Some(item)) = find(account) {
                // SAFETY: `item` is a live reference. Deleting the keychain item leaves
                // releasing the reference to Drop.
                unsafe {
                    SecKeychainItemDelete(item.0);
                }
            }
        }
    }

    #[cfg(windows)]
    mod win {
        use windows_sys::Win32::Security::Credentials::{
            CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
            CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
        };

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        /// `target` is the full name, from [`super::target_name`].
        pub fn read(target: &str) -> Option<String> {
            let target = wide(target);
            let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `target` is NUL-terminated; on success `cred` points at a block the
            // system allocated, read here and released with CredFree.
            unsafe {
                if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                    return None;
                }
                let blob = std::slice::from_raw_parts(
                    (*cred).CredentialBlob,
                    (*cred).CredentialBlobSize as usize,
                );
                let secret = String::from_utf8(blob.to_vec()).ok();
                CredFree(cred.cast());
                secret
            }
        }

        pub fn write(target: &str, label: &str, secret: &str) -> Result<(), String> {
            if secret.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
                return Err("too long for the Windows Credential Manager".into());
            }
            let mut target = wide(target);
            let mut comment = wide(label);
            let mut user = wide("arakun");
            let mut blob = secret.as_bytes().to_vec();
            let cred = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                Comment: comment.as_mut_ptr(),
                CredentialBlobSize: blob.len() as u32,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: user.as_mut_ptr(),
                ..Default::default()
            };
            // SAFETY: every pointer in `cred` points into a buffer alive for the call.
            if unsafe { CredWriteW(&cred, 0) } == 0 {
                return Err(format!(
                    "Credential Manager: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }

        pub fn delete(target: &str) {
            let target = wide(target);
            // SAFETY: `target` is NUL-terminated.
            unsafe {
                CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0);
            }
        }
    }
}

/// The fastest refresh rate among the displays, in Hz, where the desktop says so. Only
/// Hyprland is asked for now (`hyprctl monitors`); elsewhere this is None. Only the
/// browser asks.
#[cfg(feature = "browser")]
pub fn refresh_rate() -> Option<i32> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = command("hyprctl").args(["-j", "monitors"]).output().ok()?;
    let monitors: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    monitors
        .as_array()?
        .iter()
        .filter_map(|m| m["refreshRate"].as_f64())
        .map(|hz| hz.round() as i32)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[test]
    fn login_path_parses_last_line_and_prepends_macos_defaults() {
        assert_eq!(
            parse_login_path("noise from .zshrc\n/opt/homebrew/bin:/usr/bin\n"),
            Some("/opt/homebrew/bin:/usr/bin".into())
        );
        assert_eq!(parse_login_path("no slashes here"), None);
        assert_eq!(
            parse_login_path("noise from .zshrc\n/opt/homebrew/bin:/usr/bin"),
            Some("/opt/homebrew/bin:/usr/bin".into())
        );
        let fallback = fallback_macos_path(
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Path::new("/Users/test"),
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&fallback).collect();
        assert_eq!(dirs[0], PathBuf::from("/opt/homebrew/bin"));
        assert!(dirs.contains(&PathBuf::from("/Users/test/.local/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_output_past_the_pipe_buffer_is_read() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "head -c 200000 /dev/zero | tr '\\0' x; printf '\\n%s' /opt/homebrew/bin:/usr/bin",
        ]);
        let started = Instant::now();
        let out = stdout_within(cmd, Duration::from_secs(5)).expect("finishes in time");
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(out.len(), 200_000 + "\n/opt/homebrew/bin:/usr/bin".len());
        assert_eq!(
            parse_login_path(&out),
            Some("/opt/homebrew/bin:/usr/bin".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_that_hangs_is_given_up_on() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        assert_eq!(stdout_within(cmd, Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_login_shell_path_reads_and_caches_atomically() {
        let path = read_login_shell_path().expect("login shell should return PATH on macOS");
        assert!(path.contains('/'), "unexpected PATH: {path}");
        let dir = std::env::temp_dir().join(format!("arakun-path-test-{}", std::process::id()));
        let file = dir.join("login-path.txt");
        write_login_path_cache(&file, &path).unwrap();
        let read = std::fs::read_to_string(&file).unwrap();
        assert_eq!(parse_login_path(&read), Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relaunch_opens_the_bundle_a_binary_belongs_to() {
        assert_eq!(
            app_bundle(Path::new("/Applications/Arakun.app/Contents/MacOS/arakun")),
            Some(PathBuf::from("/Applications/Arakun.app"))
        );
        assert_eq!(app_bundle(Path::new("/repo/target/debug/arakun")), None);
        assert_eq!(app_bundle(Path::new("/x/Contents/MacOS/arakun")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_past_its_limit_is_stopped_with_what_it_started() {
        let started = Instant::now();
        let err = output_within(
            Command::new("sh").args(["-c", "sleep 30 & sleep 30"]),
            None,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(err.to_string().starts_with("sh took longer than"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// git removes its lock files when it gets SIGTERM, not when it is killed.
    #[cfg(unix)]
    #[test]
    fn a_stopped_command_is_asked_to_end_first() {
        let dir = std::env::temp_dir().join(format!("arakun-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = "trap 'touch cleaned; exit 1' TERM; sleep 30 & wait";
        let err = output_within(
            Command::new("sh").args(["-c", script]).current_dir(&dir),
            None,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(dir.join("cleaned").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn what_a_finished_command_left_running_does_not_hold_it() {
        let started = Instant::now();
        let out = output_within(
            Command::new("sh").args(["-c", "echo hi; sleep 30 &"]),
            None,
            Duration::from_secs(20),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hi\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// Like the fsmonitor daemon `git status` starts: it closes the pipes and is
    /// meant to outlive the command.
    #[cfg(unix)]
    #[test]
    fn a_helper_that_lets_go_of_the_pipes_keeps_running() {
        let started = Instant::now();
        let out = output_within(
            Command::new("sh").args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"]),
            None,
            Duration::from_secs(20),
        )
        .unwrap();
        assert!(started.elapsed() < STOP_GRACE);
        let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let alive = |pid: &str| {
            Command::new("kill")
                .args(["-0", pid])
                .status()
                .is_ok_and(|s| s.success())
        };
        assert!(alive(&pid), "the helper was killed");
        let _ = Command::new("kill").arg(&pid).status();
    }

    /// The grace is for the whole group: what the command started may still be
    /// cleaning up after the command itself is gone.
    #[cfg(unix)]
    #[test]
    fn what_a_stopped_command_started_gets_the_grace_too() {
        let dir = std::env::temp_dir().join(format!("arakun-grace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = "sh -c \"trap 'sleep 0.5; touch cleaned; exit 1' TERM; \
                      sleep 30 & wait\" & wait";
        let err = output_within(
            Command::new("sh").args(["-c", script]).current_dir(&dir),
            None,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(dir.join("cleaned").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn input_goes_to_stdin_and_without_it_stdin_is_empty() {
        let out = output_within(
            &mut Command::new("cat"),
            Some(b"abc"),
            Duration::from_secs(10),
        );
        assert_eq!(out.unwrap().stdout, b"abc");
        let out = output_within(&mut Command::new("cat"), None, Duration::from_secs(10));
        assert_eq!(out.unwrap().stdout, b"");
    }

    #[test]
    fn a_stopped_command_is_named_by_its_subcommand() {
        let name = |args: &[&str]| {
            let mut cmd = Command::new("/usr/bin/git");
            cmd.args(args);
            command_name(&cmd)
        };
        assert_eq!(name(&["fetch", "--all", "--quiet"]), "git fetch");
        assert_eq!(name(&["-c", "alias.x=!sleep 1", "x"]), "git x");
        assert_eq!(name(&["show", "HEAD:src/a.rs"]), "git show");
        let mut gh = Command::new("gh");
        gh.args(["-R", "o/r", "pr", "list", "--json", "number"]);
        assert_eq!(command_name(&gh), "gh pr list");
    }

    #[test]
    fn only_a_binary_in_an_app_bundle_counts_as_bundled() {
        assert!(is_bundled(Path::new(
            "/Applications/Arakun.app/Contents/MacOS/arakun"
        )));
        assert!(!is_bundled(Path::new(
            "/Users/me/arakun/target/debug/arakun"
        )));
        assert!(!is_bundled(Path::new("/opt/Contents/MacOS/arakun")));
        assert!(!is_bundled(Path::new("/MacOS/arakun")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_shortcuts_take_cmd() {
        assert_eq!(keystroke("ctrl-k"), "cmd-k");
        assert_eq!(keystroke("ctrl-shift-t"), "cmd-shift-t");
        assert_eq!(keystroke("ctrl--"), "cmd--");
        assert_eq!(keystroke("ctrl-tab"), "ctrl-tab");
        assert_eq!(keystroke("ctrl-`"), "ctrl-`");
        assert_eq!(keystroke("ctrl-shift-`"), "ctrl-shift-`");
        assert_eq!(keystroke("alt-left"), "cmd-[");
        assert_eq!(keystroke("alt-up"), "alt-up");
        assert_eq!(
            shortcut_label("New terminal (Ctrl+Shift+T)"),
            "New terminal (⇧⌘T)"
        );
        assert_eq!(shortcut_label("Ctrl+= / Ctrl+- / Ctrl+0"), "⌘= / ⌘- / ⌘0");
        assert_eq!(shortcut_label("Ctrl+Tab / Ctrl+Shift+Tab"), "⌃Tab / ⌃⇧Tab");
        assert_eq!(shortcut_label("Back (Alt+Left)"), "Back (⌘[)");
        assert_eq!(shortcut_label("Alt+↑ / Alt+↓"), "⌥↑ / ⌥↓");
        assert_eq!(shortcut_label("Ctrl+1 … Ctrl+9"), "⌘1 … ⌘9");
        assert_eq!(shortcut_label("Settings"), "Settings");
    }

    #[test]
    fn mac_keycaps_use_apple_symbols_and_order() {
        assert_eq!(mac_keycaps(&["Ctrl", "↵"]), ["⌘", "↵"]);
        assert_eq!(mac_keycaps(&["Ctrl", "⇧", "N"]), ["⇧", "⌘", "N"]);
        assert_eq!(mac_keycaps(&["Ctrl", "Alt", "1"]), ["⌥", "⌘", "1"]);
        assert_eq!(mac_keycaps(&["Ctrl", "Tab"]), ["⌃", "Tab"]);
        assert_eq!(mac_keycaps(&["Shift", "Tab"]), ["⇧", "Tab"]);
        assert_eq!(mac_keycaps(&["↑", "↓"]), ["↑", "↓"]);
        assert_eq!(mac_keycaps(&["Esc"]), ["Esc"]);
    }

    #[test]
    fn out_of_range_fields_roll_over_like_mktime() {
        assert_eq!(normalize(2026, 9, 15, 9, 30), (2026, 9, 15, 9, 30));
        assert_eq!(
            normalize(2026, 9, 32, 0, 0),
            (2026, 10, 1, 0, 0),
            "Oct 32 is Nov 1"
        );
        assert_eq!(
            normalize(2026, 11, 32, 0, 0),
            (2027, 0, 1, 0, 0),
            "Dec 32 is Jan 1"
        );
        assert_eq!(
            normalize(2028, 1, 30, 0, 0),
            (2028, 2, 1, 0, 0),
            "a leap February has 29 days"
        );
        assert_eq!(normalize(2026, 1, 29, 0, 0), (2026, 2, 1, 0, 0));
        assert_eq!(
            normalize(2026, 9, 31, 24, 5),
            (2026, 10, 1, 0, 5),
            "hour 24 is the next day"
        );
        assert_eq!(
            normalize(2026, 12, 1, 0, 0),
            (2027, 0, 1, 0, 0),
            "month 12 is next January"
        );
        assert_eq!(
            normalize(2026, 0, 0, 0, 0),
            (2025, 11, 31, 0, 0),
            "day 0 is the day before"
        );
    }

    #[test]
    fn civil_days_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for days in [-1, 0, 59, 365, 11_016, 20_000, 30_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn local_time_and_back_agree() {
        let now = 1_791_000_000;
        let t = local_time(now).unwrap();
        let back = from_local(t.year, t.month, t.day, t.hour, t.minute).unwrap();
        assert_eq!(back, now - now % 60, "a minute's precision");
        let next = from_local(t.year, t.month, t.day + 1, t.hour, t.minute).unwrap();
        assert!(
            (next - back).abs_diff(86_400) <= 3_600,
            "a day later, give or take a DST hour"
        );
    }

    #[test]
    fn a_program_is_found_on_a_path() {
        let dir = std::env::temp_dir().join(format!("arakun-which-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let file = {
            let file = dir.join("tool.cmd");
            std::fs::write(&file, "@echo off\n").unwrap();
            file
        };
        #[cfg(not(windows))]
        let file = {
            use std::os::unix::fs::PermissionsExt as _;
            let file = dir.join("tool");
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            file
        };
        let found = find_in([dir.clone()], "tool");
        // PATHEXT lists `.CMD`, and Windows paths ignore case: `tool.CMD` is `tool.cmd`.
        #[cfg(windows)]
        let (found, file) = (
            found.map(|p| p.to_string_lossy().to_lowercase()),
            file.to_string_lossy().to_lowercase(),
        );
        assert_eq!(found, Some(file));
        assert_eq!(find_in([dir.clone()], "missing"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keyring_names_match_what_credential_manager_lists() {
        assert_eq!(
            keyring::target_name("arakun", "teams-session"),
            "arakun:teams-session"
        );
        assert_eq!(
            keyring::target_name("Factory CLI", "auth-encryption-key"),
            "Factory CLI/auth-encryption-key"
        );
    }

    /// What arakun stores it must find again, and forget on clear. Only Windows: the
    /// Secret Service needs a desktop session that CI and headless Linux lack.
    #[cfg(windows)]
    #[test]
    #[ignore = "writes to this machine's Credential Manager"]
    fn a_stored_secret_is_found_again_and_cleared() {
        let account = format!("test-roundtrip-{}", std::process::id());
        keyring::store(&account, "arakun: test", "s3cret").unwrap();
        let found = keyring::lookup(&account);
        keyring::clear(&account);
        assert_eq!(found.as_deref(), Some("s3cret"));
        assert_eq!(keyring::lookup(&account), None);
    }
}
