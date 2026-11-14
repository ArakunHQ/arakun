//! `arakun dev`: a second arakun, started from this binary inside a virtual X
//! display (Xvfb), for checking arakun's own UI. Screenshots read that display
//! and input is dispatched inside the app, so the user's screen, pointer and
//! focus are never touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gpui::{
    Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput,
    ScrollDelta, ScrollWheelEvent, TouchPhase, point, px,
};
use serde::{Deserialize, Serialize};

use crate::control::{self, Reply, Request};

pub const USAGE: &str = "\
arakun dev start [dir] [--size WxH]  start a fresh test arakun on a virtual display, with
                                  dir (default: here) as its project; restarts one
arakun dev shot [file.png]          screenshot it (window at 0,0, 1 px = 1 point)
arakun dev click <x> <y> [--right] [--double]
arakun dev click <text> [--nth N] [--right] [--double]
                                  click the text where it shows (read with OCR)
arakun dev text [words…]            the text on screen, a line each with its x y;
                                  with words, only where they show
arakun dev wait [--text T | --gone T | --idle] [--timeout S]
                                  wait until T shows, T is gone, or no session
                                  works (default --idle, 120 s)
arakun dev move <x> <y> | scroll <x> <y> <dy> [dx] [--ctrl]
arakun dev down <x> <y> | up <x> <y>  press and release apart, to drag: down, move…, up
arakun dev key <key…>               keystrokes: ctrl-f, enter, shift-tab, escape…
arakun dev type <text…>             type text where the focus is
arakun dev log                      the test arakun's output
arakun dev stop                     stop it
arakun dev <command…>               any other arakun command, against the test arakun
                                  (arakun dev open src/a.rs, arakun dev logs)";

const DEFAULT_SIZE: (u32, u32) = (1440, 900);

/// Set in the test arakun's environment: its window size, `WxH`.
const ENV: &str = "ARAKUN_DEV";

#[derive(Serialize, Deserialize, Debug)]
struct Instance {
    display: u32,
    xvfb: u32,
    arakun: u32,
    width: u32,
    height: u32,
    /// Unix seconds it started: Claude sessions born after are its own.
    #[serde(default)]
    started: u64,
}

fn root() -> PathBuf {
    control::runtime_dir().join("dev")
}

fn state_file() -> PathBuf {
    root().join("state.json")
}

/// The test arakun's control socket (see `control::socket_path`).
pub fn socket() -> PathBuf {
    state_file().with_extension("sock")
}

/// The window size (in points) when this process is a test arakun: the
/// display's pixels, divided by `GPUI_X11_SCALE_FACTOR` when it is set, so a
/// 3840x2160 display at scale 2 holds a 1920x1080 window drawn at twice the
/// density (sharp recordings).
pub fn size() -> Option<(f32, f32)> {
    let v = std::env::var(ENV).ok()?;
    let (w, h) = v.split_once('x')?;
    let scale = std::env::var("GPUI_X11_SCALE_FACTOR")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| *s > 0.)
        .unwrap_or(1.);
    Some((
        w.parse::<f32>().ok()? / scale,
        h.parse::<f32>().ok()? / scale,
    ))
}

/// `arakun dev …`; `None` hands the arguments to the normal CLI, aimed at the
/// test arakun.
pub fn cli(args: &[String]) -> Option<i32> {
    let result = match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            Ok(String::new())
        }
        Some("start") => start(&args[1..]),
        Some("stop") => stop().map(|_| "stopped the test arakun".into()),
        Some("shot") => shot(args.get(1).map(PathBuf::from)),
        Some("log") => {
            std::fs::read_to_string(root().join("arakun.log")).map_err(|e| format!("no log: {e}"))
        }
        Some("socket") => Ok(socket().display().to_string()),
        Some("click") if args.get(1).is_some_and(|a| a.parse::<f32>().is_err()) => {
            click_text(&args[1..])
        }
        Some("text") => text(&args[1..]),
        Some("wait") => wait(&args[1..]),
        Some(cmd) if INPUT.contains(&cmd) => input(args),
        Some(_) => {
            if running().is_none() {
                eprintln!("arakun: no test arakun is running; start one with `arakun dev start`");
                return Some(1);
            }
            // SAFETY: the CLI is single-threaded.
            unsafe { std::env::set_var("ARAKUN_SOCKET", socket()) };
            return None;
        }
    };
    match result {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            Some(0)
        }
        Err(e) => {
            eprintln!("arakun dev: {e}");
            Some(1)
        }
    }
}

const INPUT: &[&str] = &["click", "move", "scroll", "key", "type", "down", "up"];

fn alive(pid: u32) -> bool {
    crate::platform::process_alive(pid)
}

fn running() -> Option<Instance> {
    let bytes = std::fs::read(root().join("instance.json")).ok()?;
    let inst: Instance = serde_json::from_slice(&bytes).ok()?;
    alive(inst.arakun).then_some(inst)
}

fn stop() -> Result<(), String> {
    let Ok(bytes) = std::fs::read(root().join("instance.json")) else {
        return Ok(());
    };
    if let Ok(inst) = serde_json::from_slice::<Instance>(&bytes) {
        for pid in [inst.arakun, inst.xvfb] {
            if alive(pid) {
                crate::platform::terminate(pid);
            }
        }
        let start = Instant::now();
        while (alive(inst.arakun) || alive(inst.xvfb)) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if inst.started > 0 {
            archive_test_sessions(inst.started);
        }
    }
    let _ = std::fs::remove_file(root().join("instance.json"));
    Ok(())
}

/// Archives, in Claude Code, the sessions the test arakun started, so tests
/// don't fill the user's /resume and arakun's import list. It is Claude Code's
/// own archive (a `[archived] …` title), undone by renaming. Sessions it only
/// opened (imported) were born before it and are left alone.
fn archive_test_sessions(started: u64) {
    let dir = root().join("data").join("arakun").join("sessions");
    for path in std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
    {
        let Some(meta) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            continue;
        };
        let meta = &meta["meta"];
        let (Some("claude"), Some(id), Some(cwd)) = (
            meta["agent"].as_str(),
            meta["session_id"].as_str(),
            meta["cwd"].as_str(),
        ) else {
            continue;
        };
        let file =
            crate::agent::catalog::claude_project_dir(Path::new(cwd)).join(format!("{id}.jsonl"));
        if born(&file).is_none_or(|t| t + 5 < started) {
            continue;
        }
        let title = meta["title"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or("arakun dev test");
        let line = serde_json::json!({"type": "custom-title", "customTitle": format!("[archived] {title}"), "sessionId": id});
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&file) {
            let _ = std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes());
        }
    }
}

/// When a Claude Code session began: its transcript's first timestamp.
fn born(file: &Path) -> Option<u64> {
    use std::io::Read as _;
    let mut head = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    let head = String::from_utf8_lossy(&head);
    let at = head.find("\"timestamp\":\"")? + 13;
    let end = head[at..].find('"')? + at;
    crate::agent::plan::from_iso(&head[at..end])
}

/// `[dir] [--size WxH]`.
fn start_args(args: &[String]) -> Result<(Option<String>, (u32, u32)), String> {
    let (mut dir, mut size) = (None, DEFAULT_SIZE);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--size" {
            let v = it.next().ok_or("--size needs WxH")?;
            size = v
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                .filter(|&(w, h): &(u32, u32)| {
                    (800..=4000).contains(&w) && (520..=3000).contains(&h)
                })
                .ok_or_else(|| format!("bad --size {v}: WxH, at least 800x520"))?;
        } else if dir.is_none() {
            dir = Some(arg.clone());
        } else {
            return Err(format!("unexpected {arg}"));
        }
    }
    Ok((dir, size))
}

fn free_display() -> Option<u32> {
    (90..200).find(|n| {
        !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
    })
}

/// Starts `cmd` in a session of its own, so it outlives this command and the
/// shell that ran it.
fn spawn_detached(cmd: &mut Command, log: &Path) -> std::io::Result<u32> {
    let out = std::fs::File::create(log)?;
    crate::platform::detach(cmd);
    Ok(cmd
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .spawn()?
        .id())
}

fn start(args: &[String]) -> Result<String, String> {
    if !cfg!(target_os = "linux") {
        return Err("arakun dev runs arakun on an Xvfb display, which only Linux has".into());
    }
    let (dir, (width, height)) = start_args(args)?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let project = cwd
        .join(dir.as_deref().unwrap_or("."))
        .canonicalize()
        .map_err(|e| format!("project: {e}"))?;
    if !project.is_dir() {
        return Err(format!("{} is not a directory", project.display()));
    }
    stop()?;
    let root = root();
    for sub in ["data", "run"] {
        let _ = std::fs::remove_dir_all(root.join(sub));
    }
    let _ = std::fs::remove_file(socket());
    for sub in ["data/arakun", "run", "shots"] {
        std::fs::create_dir_all(root.join(sub)).map_err(|e| e.to_string())?;
    }
    let path = project.display().to_string();
    let state = serde_json::json!({
        "projects": [{
            "path": path, "logo": 4906624, "art": 1, "setup": null, "notes": [], "monos": [],
            "automations": [], "active_env": path,
            "envs": [{"path": path, "branch": "main", "is_main": true, "agent": "droid",
                      "settings": [], "sessions": []}],
        }],
        "active_project": 0, "dock_open": false, "dock_height": 200, "rail_open": true,
        "panel_open": true, "arcade": false,
    });
    std::fs::write(state_file(), state.to_string()).map_err(|e| e.to_string())?;

    let display = free_display().ok_or("no free X display number")?;
    let xvfb = spawn_detached(
        Command::new("Xvfb").args([
            format!(":{display}"),
            "-screen".into(),
            "0".into(),
            format!("{width}x{height}x24"),
            "-nolisten".into(),
            "tcp".into(),
        ]),
        &root.join("xvfb.log"),
    )
    .map_err(|e| format!("could not start Xvfb ({e}); install it (xorg-server-xvfb / xvfb)"))?;
    let up = Instant::now();
    while !Path::new(&format!("/tmp/.X11-unix/X{display}")).exists() {
        if up.elapsed() > Duration::from_secs(5) || !alive(xvfb) {
            return Err(
                "Xvfb did not start; see its log in `arakun dev socket`'s directory".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let arakun = spawn_detached(
        Command::new(exe)
            .current_dir(&project)
            .env_remove("ARAKUN_SOCKET")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env("DISPLAY", format!(":{display}"))
            .env("ARAKUN_STATE", state_file())
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("ARAKUN_CI", "off")
            .env(ENV, format!("{width}x{height}")),
        &root.join("arakun.log"),
    )
    .map_err(|e| e.to_string())?;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let inst = Instance {
        display,
        xvfb,
        arakun,
        width,
        height,
        started,
    };
    std::fs::write(
        root.join("instance.json"),
        serde_json::to_vec(&inst).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;

    let ping = Request {
        cmd: "ls".into(),
        args: Vec::new(),
        cwd: project.clone(),
        side: false,
    };
    let up = Instant::now();
    loop {
        if control::send_to(&socket(), &ping).is_ok() {
            break;
        }
        if !alive(arakun) {
            return Err("the test arakun exited; `arakun dev log` shows why".into());
        }
        if up.elapsed() > Duration::from_secs(30) {
            return Err(
                "the test arakun did not answer within 30 s; `arakun dev log` shows its output"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = focus(display);
    // The first frames are still being laid out when the socket answers.
    std::thread::sleep(Duration::from_millis(600));
    Ok(format!(
        "test arakun running on display :{display} ({width}x{height}) with {path}\n\
         `arakun dev shot` screenshots it; `arakun dev stop` ends it"
    ))
}

/// Gives the app's window the X input focus, so it acts as the active window
/// (no window manager runs on the virtual display to do it).
fn focus(display: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, InputFocus, MapState};
    let (conn, screen) = x11rb::connect(Some(&format!(":{display}")))?;
    let root = conn.setup().roots[screen].root;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        for w in conn.query_tree(root)?.reply()?.children {
            if conn.get_window_attributes(w)?.reply()?.map_state == MapState::VIEWABLE {
                conn.set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME)?;
                conn.flush()?;
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn shot(file: Option<PathBuf>) -> Result<String, String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
    let inst = running().ok_or("no test arakun is running; start one with `arakun dev start`")?;
    let (conn, screen) =
        x11rb::connect(Some(&format!(":{}", inst.display))).map_err(|e| e.to_string())?;
    let screen = &conn.setup().roots[screen];
    let (w, h) = (screen.width_in_pixels, screen.height_in_pixels);
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, screen.root, 0, 0, w, h, !0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let rgb = bgrx_to_rgb(&reply.data, w as usize * h as usize).ok_or("unexpected image format")?;
    let img = image::RgbImage::from_raw(w as u32, h as u32, rgb).ok_or("bad image size")?;
    let file = file.unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        root().join("shots").join(format!("shot-{secs}.png"))
    });
    img.save(&file).map_err(|e| e.to_string())?;
    Ok(file.display().to_string())
}

// ---- reading the screen ----------------------------------------------------

/// A run of words tesseract read on one line, in window points.
#[derive(Debug, Clone, PartialEq)]
struct Found {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Found {
    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }
}

/// OCR scale: UI text is small for tesseract at 1 px a point.
const OCR_SCALE: f32 = 3.;

/// The words on screen, grouped in lines. The shot is made gray, light on dark
/// turned dark on light, and tripled (dimmed text under a dialog needs it), which is what tesseract reads best.
fn screen_lines() -> Result<Vec<Vec<Found>>, String> {
    let raw = PathBuf::from(shot(Some(root().join("ocr-shot.png")))?);
    let mut img = image::open(&raw).map_err(|e| e.to_string())?.to_luma8();
    let mean = img.pixels().map(|p| p.0[0] as u64).sum::<u64>() / (img.len() as u64).max(1);
    if mean < 128 {
        image::imageops::invert(&mut img);
    }
    let (w, h) = img.dimensions();
    let big = image::imageops::resize(
        &img,
        (w as f32 * OCR_SCALE) as u32,
        (h as f32 * OCR_SCALE) as u32,
        image::imageops::FilterType::Triangle,
    );
    let input = root().join("ocr-in.png");
    big.save(&input).map_err(|e| e.to_string())?;
    let out = Command::new("tesseract")
        .arg(&input)
        .args(["stdout", "--psm", "11", "tsv"])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("tesseract: {e} (install it to read the screen)"))?;
    Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout), OCR_SCALE))
}

/// Tesseract's TSV: word rows (level 5) with their block, paragraph and line.
fn parse_tsv(tsv: &str, scale: f32) -> Vec<Vec<Found>> {
    let mut lines: Vec<((u32, u32, u32), Vec<Found>)> = Vec::new();
    for row in tsv.lines().skip(1) {
        let c: Vec<&str> = row.split('\t').collect();
        // Low confidence is mostly icons and art read as letters.
        if c.len() < 12
            || c[0] != "5"
            || c[11].trim().is_empty()
            || c[10].parse::<f32>().unwrap_or(0.) < 50.
        {
            continue;
        }
        let n = |i: usize| c[i].parse::<f32>().unwrap_or(0.) / scale;
        let key = (
            c[2].parse().unwrap_or(0),
            c[3].parse().unwrap_or(0),
            c[4].parse().unwrap_or(0),
        );
        let word = Found {
            text: c[11].trim().to_string(),
            x: n(6),
            y: n(7),
            w: n(8),
            h: n(9),
        };
        match lines.last_mut() {
            Some((k, words)) if *k == key => words.push(word),
            _ => lines.push((key, vec![word])),
        }
    }
    lines.into_iter().map(|(_, words)| words).collect()
}

fn norm(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Where the words of `query` show in a row, the last one allowed to be cut
/// ("sessions…" reads as "sessions").
fn find_in(lines: &[Vec<Found>], query: &str) -> Vec<Found> {
    let want: Vec<String> = query
        .split_whitespace()
        .map(norm)
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = Vec::new();
    if want.is_empty() {
        return out;
    }
    for words in lines {
        let got: Vec<String> = words.iter().map(|w| norm(&w.text)).collect();
        for start in 0..got.len() {
            let fits = want.iter().enumerate().all(|(i, t)| {
                got.get(start + i)
                    .is_some_and(|g| g == t || (i + 1 == want.len() && g.starts_with(t.as_str())))
            });
            if fits {
                out.push(join(&words[start..start + want.len()]));
            }
        }
    }
    out
}

fn join(words: &[Found]) -> Found {
    let x = words.iter().map(|w| w.x).fold(f32::MAX, f32::min);
    let y = words.iter().map(|w| w.y).fold(f32::MAX, f32::min);
    let right = words.iter().map(|w| w.x + w.w).fold(0., f32::max);
    let bottom = words.iter().map(|w| w.y + w.h).fold(0., f32::max);
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Found {
        text,
        x,
        y,
        w: right - x,
        h: bottom - y,
    }
}

/// `arakun dev text [words…]`.
fn text(args: &[String]) -> Result<String, String> {
    let lines = screen_lines()?;
    let found = if args.is_empty() {
        lines.iter().map(|l| join(l)).collect()
    } else {
        find_in(&lines, &args.join(" "))
    };
    if found.is_empty() {
        return Err(format!("“{}” is not on screen", args.join(" ")));
    }
    Ok(found
        .iter()
        .map(|f| {
            let (x, y) = f.center();
            format!("{x:>6.0} {y:>5.0}  {}", f.text)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `arakun dev click <text> [--nth N] [--right] [--double]`.
fn click_text(args: &[String]) -> Result<String, String> {
    let (mut words, mut flags, mut nth) = (Vec::new(), Vec::new(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--nth" => {
                nth = Some(
                    it.next()
                        .and_then(|n| n.parse::<usize>().ok())
                        .filter(|n| *n > 0)
                        .ok_or("--nth wants 1, 2…")?,
                )
            }
            "--right" | "--double" => flags.push(a.clone()),
            _ => words.push(a.as_str()),
        }
    }
    let query = words.join(" ");
    let found = find_in(&screen_lines()?, &query);
    let pick = match (found.len(), nth) {
        (0, _) => {
            return Err(format!(
                "“{query}” is not on screen (see `arakun dev text`)"
            ));
        }
        (_, Some(n)) => found
            .get(n - 1)
            .ok_or(format!("“{query}” shows {} times", found.len()))?,
        (1, None) => &found[0],
        (n, None) => {
            let list: Vec<String> = found
                .iter()
                .map(|f| format!("  {:.0} {:.0}", f.center().0, f.center().1))
                .collect();
            return Err(format!(
                "“{query}” shows {n} times; pick one with --nth:\n{}",
                list.join("\n")
            ));
        }
    };
    let (x, y) = pick.center();
    let mut click = vec!["click".to_string(), format!("{x:.0}"), format!("{y:.0}")];
    click.extend(flags);
    input(&click)?;
    Ok(format!("clicked “{}” at {x:.0} {y:.0}", pick.text))
}

/// `arakun dev wait [--text T | --gone T | --idle] [--timeout S]`.
fn wait(args: &[String]) -> Result<String, String> {
    enum Until {
        Text(String),
        Gone(String),
        Idle,
    }
    let (mut until, mut timeout) = (Until::Idle, 120u64);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} wants a value"));
        match a.as_str() {
            "--text" => until = Until::Text(value()?),
            "--gone" => until = Until::Gone(value()?),
            "--idle" => until = Until::Idle,
            "--timeout" => timeout = value()?.parse().map_err(|_| "--timeout wants seconds")?,
            other => return Err(format!("unknown wait flag {other}")),
        }
    }
    running().ok_or("no test arakun is running; start one with `arakun dev start`")?;
    let start = Instant::now();
    let mut seen_work = false;
    loop {
        let elapsed = start.elapsed();
        let done = match &until {
            Until::Text(t) => {
                (!find_in(&screen_lines()?, t).is_empty()).then(|| format!("“{t}” shows"))
            }
            Until::Gone(t) => find_in(&screen_lines()?, t)
                .is_empty()
                .then(|| format!("“{t}” is gone")),
            Until::Idle => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                let req = Request {
                    cmd: "ls".into(),
                    args: Vec::new(),
                    cwd,
                    side: false,
                };
                let ls = control::send_to(&socket(), &req)
                    .map(|r| r.message)
                    .unwrap_or_default();
                let working = ls.contains(" working");
                seen_work |= working;
                // A prompt just sent may still be starting its agent: give it a moment.
                (!working && (seen_work || elapsed > Duration::from_secs(5))).then(|| {
                    if ls.contains("needs input") {
                        "a session needs input".to_string()
                    } else {
                        "no session is working".to_string()
                    }
                })
            }
        };
        if let Some(what) = done {
            std::thread::sleep(Duration::from_millis(250));
            return Ok(format!("{what} ({:.1} s)", elapsed.as_secs_f32()));
        }
        if elapsed > Duration::from_secs(timeout) {
            return Err(format!("still waiting after {timeout} s"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A 24-bit X image comes as 4 bytes a pixel, blue first.
fn bgrx_to_rgb(data: &[u8], pixels: usize) -> Option<Vec<u8>> {
    (data.len() >= pixels * 4).then(|| {
        data.as_chunks::<4>()
            .0
            .iter()
            .take(pixels)
            .flat_map(|&[b, g, r, _]| [r, g, b])
            .collect()
    })
}

fn input(args: &[String]) -> Result<String, String> {
    parse_input(args)?;
    if running().is_none() {
        return Err("no test arakun is running; start one with `arakun dev start`".into());
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let req = Request {
        cmd: "dev".into(),
        args: args.to_vec(),
        cwd,
        side: false,
    };
    let reply = control::send_to(&socket(), &req)
        .map_err(|e| format!("could not reach the test arakun: {e}"))?;
    // Let the frame the input caused be drawn before a screenshot.
    std::thread::sleep(Duration::from_millis(250));
    if reply.ok {
        Ok(reply.message)
    } else {
        Err(reply.message)
    }
}

/// One step of `arakun dev` input.
#[derive(Debug, PartialEq)]
pub enum Synth {
    Move(f32, f32),
    /// The left button goes down here and stays down: moves drag until `Up`.
    Down(f32, f32),
    Up(f32, f32),
    Click {
        x: f32,
        y: f32,
        button: MouseButton,
        count: usize,
    },
    Scroll {
        x: f32,
        y: f32,
        dy: f32,
        dx: f32,
        ctrl: bool,
    },
    Key(String),
    Text(String),
}

pub fn parse_input(args: &[String]) -> Result<Vec<Synth>, String> {
    let num = |s: Option<&String>, what: &str| {
        s.and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("{what} must be a number"))
    };
    let rest = &args[1.min(args.len())..];
    match args.first().map(String::as_str) {
        Some("click") => {
            let (x, y) = (num(rest.first(), "x")?, num(rest.get(1), "y")?);
            let flags = &rest[2.min(rest.len())..];
            let mut button = MouseButton::Left;
            let mut count = 1;
            for f in flags {
                match f.as_str() {
                    "--right" => button = MouseButton::Right,
                    "--double" => count = 2,
                    other => return Err(format!("unknown click flag {other}")),
                }
            }
            Ok(vec![Synth::Click {
                x,
                y,
                button,
                count,
            }])
        }
        Some("move") if rest.len() == 2 => Ok(vec![Synth::Move(
            num(rest.first(), "x")?,
            num(rest.get(1), "y")?,
        )]),
        Some("down") if rest.len() == 2 => Ok(vec![Synth::Down(
            num(rest.first(), "x")?,
            num(rest.get(1), "y")?,
        )]),
        Some("up") if rest.len() == 2 => Ok(vec![Synth::Up(
            num(rest.first(), "x")?,
            num(rest.get(1), "y")?,
        )]),
        Some("scroll") => {
            let ctrl = rest.iter().any(|a| a == "--ctrl");
            let rest: Vec<&String> = rest.iter().filter(|a| *a != "--ctrl").collect();
            if rest.len() != 3 && rest.len() != 4 {
                return Err("usage: scroll <x> <y> <dy> [dx] [--ctrl]".into());
            }
            Ok(vec![Synth::Scroll {
                x: num(rest.first().copied(), "x")?,
                y: num(rest.get(1).copied(), "y")?,
                dy: num(rest.get(2).copied(), "dy")?,
                dx: if rest.len() == 4 {
                    num(rest.get(3).copied(), "dx")?
                } else {
                    0.
                },
                ctrl,
            }])
        }
        Some("key") if !rest.is_empty() => rest
            .iter()
            .map(|k| {
                Keystroke::parse(k)
                    .map(|_| Synth::Key(k.clone()))
                    .map_err(|_| format!("unknown key {k}"))
            })
            .collect(),
        Some("type") if !rest.is_empty() => Ok(vec![Synth::Text(rest.join(" "))]),
        Some(cmd) => Err(format!("usage: see `arakun dev help` ({cmd})")),
        None => Err("no input".into()),
    }
}

thread_local! {
    /// The left button is held since a `down`, so moves in between drag.
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Plays `steps` in the window, as if the user did them.
pub fn play(steps: Vec<Synth>, window: &mut gpui::Window, cx: &mut gpui::App) {
    // Steps come in screenshot pixels; the window works in points.
    let scale = window.scale_factor();
    let at = |x: f32, y: f32| point(px(x / scale), px(y / scale));
    let mods = Modifiers::default();
    for step in steps {
        match step {
            Synth::Move(x, y) => {
                let pressed_button = HELD.get().then_some(MouseButton::Left);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: at(x, y),
                        pressed_button,
                        modifiers: mods,
                    }),
                    cx,
                );
            }
            Synth::Down(x, y) => {
                HELD.set(true);
                let position = at(x, y);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: None,
                        modifiers: mods,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: mods,
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
            }
            Synth::Up(x, y) => {
                HELD.set(false);
                let position = at(x, y);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: mods,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: mods,
                        click_count: 1,
                    }),
                    cx,
                );
            }
            Synth::Click {
                x,
                y,
                button,
                count,
            } => {
                let position = at(x, y);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: None,
                        modifiers: mods,
                    }),
                    cx,
                );
                for click_count in 1..=count {
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers: mods,
                            click_count,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers: mods,
                            click_count,
                        }),
                        cx,
                    );
                }
            }
            Synth::Scroll { x, y, dy, dx, ctrl } => {
                window.dispatch_event(
                    PlatformInput::ScrollWheel(ScrollWheelEvent {
                        position: at(x, y),
                        delta: ScrollDelta::Pixels(point(px(-dx), px(-dy))),
                        modifiers: Modifiers {
                            control: ctrl,
                            ..mods
                        },
                        touch_phase: TouchPhase::Moved,
                    }),
                    cx,
                );
            }
            Synth::Key(k) => {
                if let Ok(k) = Keystroke::parse(&k) {
                    window.dispatch_keystroke(k, cx);
                }
            }
            Synth::Text(text) => {
                for ch in text.chars() {
                    let key = match ch {
                        ' ' => "space".to_string(),
                        '\n' => "enter".to_string(),
                        '\t' => "tab".to_string(),
                        c => c.to_string(),
                    };
                    let key_char = Some(ch.to_string());
                    window.dispatch_keystroke(
                        Keystroke {
                            modifiers: mods,
                            key,
                            key_char,
                        },
                        cx,
                    );
                }
            }
        }
    }
    window.refresh();
}

/// Answers a `dev` request: only a test arakun takes synthetic input.
pub fn handle(req: &Request) -> Result<Vec<Synth>, Reply> {
    if size().is_none() {
        return Err(Reply::err(
            "only a test arakun (`arakun dev start`) takes synthetic input",
        ));
    }
    parse_input(&req.args).map_err(Reply::err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn finds_phrases_in_tesseract_lines() {
        let row = |line: u32, x: u32, conf: u32, text: &str| {
            format!("5\t1\t1\t1\t{line}\t1\t{x}\t30\t60\t30\t{conf}\t{text}")
        };
        let tsv = [
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext".to_string(),
            row(1, 300, 95, "Import"),
            row(1, 390, 95, "sessions…"),
            row(1, 480, 20, "®"),
            row(2, 300, 95, "Import"),
        ]
        .join("\n");
        let lines = parse_tsv(&tsv, 3.);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 2, "low confidence words are dropped");
        let found = find_in(&lines, "import Sessions");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Import sessions…");
        assert_eq!((found[0].x, found[0].y), (100., 10.));
        assert_eq!(found[0].center(), (125., 15.));
        assert_eq!(find_in(&lines, "import").len(), 2);
        assert!(find_in(&lines, "export").is_empty());
    }

    #[test]
    fn input_parses_like_the_usage_says() {
        assert_eq!(
            parse_input(&s(&["click", "10", "20.5"])),
            Ok(vec![Synth::Click {
                x: 10.,
                y: 20.5,
                button: MouseButton::Left,
                count: 1
            }])
        );
        assert_eq!(
            parse_input(&s(&["click", "1", "2", "--right", "--double"])),
            Ok(vec![Synth::Click {
                x: 1.,
                y: 2.,
                button: MouseButton::Right,
                count: 2
            }])
        );
        assert!(parse_input(&s(&["click", "x", "2"])).is_err());
        assert!(parse_input(&s(&["click", "1"])).is_err());
        assert!(parse_input(&s(&["click", "1", "2", "--triple"])).is_err());
        assert_eq!(
            parse_input(&s(&["scroll", "5", "6", "-120"])),
            Ok(vec![Synth::Scroll {
                x: 5.,
                y: 6.,
                dy: -120.,
                dx: 0.,
                ctrl: false
            }])
        );
        assert_eq!(
            parse_input(&s(&["key", "ctrl-f", "enter"])),
            Ok(vec![
                Synth::Key("ctrl-f".into()),
                Synth::Key("enter".into())
            ])
        );
        assert!(parse_input(&s(&["key"])).is_err());
        assert_eq!(
            parse_input(&s(&["type", "hola", "mundo"])),
            Ok(vec![Synth::Text("hola mundo".into())])
        );
        assert_eq!(
            parse_input(&s(&["down", "5", "6"])),
            Ok(vec![Synth::Down(5., 6.)])
        );
        assert_eq!(
            parse_input(&s(&["up", "7", "8"])),
            Ok(vec![Synth::Up(7., 8.)])
        );
        assert!(parse_input(&s(&["down", "5"])).is_err());
        assert!(parse_input(&s(&["move", "1"])).is_err());
    }

    #[test]
    fn start_takes_a_dir_and_a_size() {
        assert_eq!(start_args(&[]), Ok((None, DEFAULT_SIZE)));
        assert_eq!(
            start_args(&s(&["--size", "1200x800", "proj"])),
            Ok((Some("proj".into()), (1200, 800)))
        );
        assert!(start_args(&s(&["--size", "10x10"])).is_err());
        assert!(start_args(&s(&["--size"])).is_err());
        assert!(start_args(&s(&["a", "b"])).is_err());
    }

    #[test]
    fn x_pixels_become_rgb() {
        assert_eq!(
            bgrx_to_rgb(&[1, 2, 3, 0, 4, 5, 6, 0], 2),
            Some(vec![3, 2, 1, 6, 5, 4])
        );
        assert_eq!(bgrx_to_rgb(&[1, 2, 3], 1), None);
    }
}
