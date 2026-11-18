//! "Keep running in the background" (Settings › General, Linux): closing the
//! window hides it, so agents, terminals and the island go on, and `arakun`
//! (or `arakun show`) brings it back. Off, closing the window quits, as before.
//!
//! GPUI can't hide a window on Linux, and the shell's state is bound to its
//! window, so the window stays open and is put out of sight instead: on
//! Hyprland in a special workspace, on Sway in the scratchpad, elsewhere it is
//! minimized.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, Window};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// The window was put away by `hide`, so `show` has to bring it back.
static HIDDEN: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    cfg!(target_os = "linux") && ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// The window's close button and the compositor's close: hides the window
/// when the setting is on, otherwise quits.
pub fn close(window: &mut Window, cx: &mut App) {
    if enabled() {
        hide(window);
    } else {
        cx.quit();
    }
}

/// Wired to `Window::on_window_should_close`: `false` keeps the window.
pub fn should_close(window: &mut Window) -> bool {
    if !enabled() {
        return true;
    }
    hide(window);
    false
}

fn hide(window: &mut Window) {
    HIDDEN.store(true, Ordering::Relaxed);
    let pid = std::process::id();
    match Desktop::current() {
        Desktop::Hyprland => hyprland(move || {
            vec![Dispatch::Move {
                workspace: format!("special:{SPECIAL}"),
                follow: false,
                pid,
            }]
        }),
        Desktop::Sway => queue(move || swaymsg(pid, "move scratchpad")),
        Desktop::Other => window.minimize_window(),
    }
}

/// Brings the window back and to the front, from wherever `hide` put it.
pub fn show(window: &mut Window) {
    let pid = std::process::id();
    if HIDDEN.swap(false, Ordering::Relaxed) {
        match Desktop::current() {
            // To the workspace the user is on now, not the one it was closed on.
            // Asking for it waits on hyprctl, so that happens off the UI thread too.
            Desktop::Hyprland => hyprland(move || {
                let mut steps = Vec::new();
                if let Some(ws) = active_workspace() {
                    steps.push(Dispatch::Move {
                        workspace: ws.to_string(),
                        follow: true,
                        pid,
                    });
                }
                steps.push(Dispatch::Focus { pid });
                steps
            }),
            // `scratchpad show` toggles: it would hide a window the user
            // already brought back with their own binding.
            Desktop::Sway => queue(move || {
                if in_scratchpad(pid) == Some(false) {
                    swaymsg(pid, "focus");
                } else {
                    swaymsg(pid, "scratchpad show, floating disable");
                }
            }),
            Desktop::Other => {}
        }
    }
    window.activate_window();
}

/// Hyprland's special workspace the hidden window waits in.
const SPECIAL: &str = "arakun";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Desktop {
    Hyprland,
    Sway,
    Other,
}

impl Desktop {
    fn current() -> Self {
        Self::from_env(|k| std::env::var_os(k).is_some())
    }

    fn from_env(set: impl Fn(&str) -> bool) -> Self {
        if !cfg!(target_os = "linux") {
            Desktop::Other
        } else if set("HYPRLAND_INSTANCE_SIGNATURE") {
            Desktop::Hyprland
        } else if set("SWAYSOCK") {
            Desktop::Sway
        } else {
            Desktop::Other
        }
    }
}

/// A Hyprland dispatch on arakun's own window, found by its pid.
#[derive(Clone, PartialEq, Debug)]
enum Dispatch {
    Move {
        workspace: String,
        follow: bool,
        pid: u32,
    },
    Focus {
        pid: u32,
    },
}

impl Dispatch {
    /// Hyprland 0.56 and later: a Lua call.
    fn lua(&self) -> String {
        match self {
            Dispatch::Move {
                workspace,
                follow,
                pid,
            } => format!(
                "hl.dsp.window.move({{ workspace = \"{workspace}\", follow = {follow}, window = \"pid:{pid}\" }})"
            ),
            Dispatch::Focus { pid } => format!("hl.dsp.focus({{ window = \"pid:{pid}\" }})"),
        }
    }

    /// Hyprland before Lua: a dispatcher and its argument.
    fn legacy(&self) -> [String; 2] {
        match self {
            Dispatch::Move {
                workspace,
                follow,
                pid,
            } => [
                if *follow {
                    "movetoworkspace"
                } else {
                    "movetoworkspacesilent"
                }
                .into(),
                format!("{workspace},pid:{pid}"),
            ],
            Dispatch::Focus { pid } => ["focuswindow".into(), format!("pid:{pid}")],
        }
    }
}

/// Whether hyprctl took the dispatch: it answers `ok`, or a warning such as
/// "window not found"; a syntax it doesn't know is an error.
fn accepted(out: &std::process::Output) -> bool {
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim_start();
    out.status.success() && (text.starts_with("ok") || text.starts_with("warning"))
}

type Job = Box<dyn FnOnce() + Send>;

/// A thread that runs what it is sent, one job after the other.
fn worker() -> std::sync::mpsc::Sender<Job> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    std::thread::spawn(move || {
        for job in rx {
            job();
        }
    });
    tx
}

/// Runs `job` off the UI thread, after the jobs queued before it. A hide and
/// a show right after it each wait on the compositor's tool; on threads of
/// their own the show could finish first, and the hide would then put the
/// window away again.
fn queue(job: impl FnOnce() + Send + 'static) {
    static WORKER: std::sync::OnceLock<std::sync::mpsc::Sender<Job>> = std::sync::OnceLock::new();
    let _ = WORKER.get_or_init(worker).send(Box::new(job));
}

/// Runs the dispatches in order, off the UI thread: each in the Lua syntax
/// first, then the older one if hyprctl turns that down. `steps` runs there
/// too, so it may ask hyprctl what to dispatch.
fn hyprland(steps: impl FnOnce() -> Vec<Dispatch> + Send + 'static) {
    queue(move || {
        for step in steps() {
            let run = |args: &[&str]| {
                std::process::Command::new("hyprctl")
                    .arg("dispatch")
                    .args(args)
                    .output()
            };
            let lua = step.lua();
            match run(&[&lua]) {
                Ok(out) if accepted(&out) => continue,
                Err(e) => {
                    eprintln!("arakun: hyprctl: {e}");
                    return;
                }
                Ok(_) => {}
            }
            let [dispatcher, arg] = step.legacy();
            match run(&[&dispatcher, &arg]) {
                Ok(out) if accepted(&out) => {}
                Ok(out) => eprintln!(
                    "arakun: hyprctl dispatch {dispatcher} {arg}: {}",
                    String::from_utf8_lossy(&out.stdout).trim()
                ),
                Err(e) => eprintln!("arakun: hyprctl: {e}"),
            }
        }
    });
}

/// Runs a Sway command on arakun's own window and waits for it; call it from
/// a queued job.
fn swaymsg(pid: u32, command: &str) {
    match std::process::Command::new("swaymsg")
        .args([format!("[pid={pid}]"), command.into()])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "arakun: swaymsg {command}: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        ),
        Err(e) => eprintln!("arakun: swaymsg: {e}"),
    }
}

/// Whether Sway holds the window of `pid` in its scratchpad, hidden. `None`
/// when Sway doesn't say.
fn in_scratchpad(pid: u32) -> Option<bool> {
    let out = std::process::Command::new("swaymsg")
        .args(["-t", "get_tree"])
        .output()
        .ok()?;
    let tree = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()?;
    scratchpad_holds(&tree, pid)
}

/// Sway keeps hidden scratchpad windows in the workspace `__i3_scratch`.
fn scratchpad_holds(tree: &serde_json::Value, pid: u32) -> Option<bool> {
    fn children(node: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
        ["nodes", "floating_nodes"]
            .into_iter()
            .filter_map(|k| node.get(k)?.as_array())
            .flatten()
    }
    fn find<'a>(
        node: &'a serde_json::Value,
        is: &dyn Fn(&serde_json::Value) -> bool,
    ) -> Option<&'a serde_json::Value> {
        if is(node) {
            return Some(node);
        }
        children(node).find_map(|c| find(c, is))
    }
    let scratch = find(tree, &|n| {
        n.get("name").and_then(|v| v.as_str()) == Some("__i3_scratch")
    })?;
    let ours = |n: &serde_json::Value| n.get("pid").and_then(|v| v.as_u64()) == Some(pid.into());
    Some(find(scratch, &ours).is_some())
}

/// The id of the workspace the user is on, from `hyprctl activeworkspace -j`.
fn active_workspace() -> Option<i64> {
    let out = std::process::Command::new("hyprctl")
        .args(["activeworkspace", "-j"])
        .output()
        .ok()?;
    workspace_id(&out.stdout)
}

fn workspace_id(json: &[u8]) -> Option<i64> {
    serde_json::from_slice::<serde_json::Value>(json)
        .ok()?
        .get("id")?
        .as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_id_reads_hyprctl_json() {
        let json = br#"{"id": 3, "name": "3", "monitor": "DP-1", "windows": 2}"#;
        assert_eq!(workspace_id(json), Some(3));
        assert_eq!(workspace_id(b"not json"), None);
        assert_eq!(workspace_id(br#"{"name": "x"}"#), None);
    }

    #[test]
    fn a_slow_hide_still_runs_before_the_show_after_it() {
        let jobs = worker();
        let (done, order) = std::sync::mpsc::channel();
        let (hide, show) = (done.clone(), done);
        let slow: Job = Box::new(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            hide.send("hide").unwrap();
        });
        jobs.send(slow).unwrap();
        jobs.send(Box::new(move || show.send("show").unwrap()))
            .unwrap();
        assert_eq!(order.recv().unwrap(), "hide");
        assert_eq!(order.recv().unwrap(), "show");
    }

    #[test]
    fn scratchpad_holds_only_a_window_sway_hid() {
        let tree = serde_json::json!({"name": "root", "nodes": [
            {"name": "__i3", "nodes": [{"name": "__i3_scratch", "nodes": [],
                "floating_nodes": [{"name": "a", "pid": 7, "nodes": []}]}]},
            {"name": "DP-1", "nodes": [{"name": "1", "floating_nodes": [],
                "nodes": [{"name": "b", "pid": 9}]}]},
        ]});
        assert_eq!(scratchpad_holds(&tree, 7), Some(true));
        // Shown again by the user's own binding: on a workspace.
        assert_eq!(scratchpad_holds(&tree, 9), Some(false));
        assert_eq!(
            scratchpad_holds(&serde_json::json!({"name": "root"}), 7),
            None
        );
    }

    #[test]
    fn dispatches_in_both_syntaxes() {
        let hide = Dispatch::Move {
            workspace: "special:arakun".into(),
            follow: false,
            pid: 42,
        };
        assert_eq!(
            hide.lua(),
            r#"hl.dsp.window.move({ workspace = "special:arakun", follow = false, window = "pid:42" })"#
        );
        assert_eq!(
            hide.legacy(),
            [
                "movetoworkspacesilent".to_string(),
                "special:arakun,pid:42".into()
            ]
        );
        let focus = Dispatch::Focus { pid: 42 };
        assert_eq!(focus.lua(), r#"hl.dsp.focus({ window = "pid:42" })"#);
        assert_eq!(focus.legacy(), ["focuswindow".to_string(), "pid:42".into()]);
    }

    #[test]
    fn desktop_comes_from_the_environment() {
        let hypr = Desktop::from_env(|k| k == "HYPRLAND_INSTANCE_SIGNATURE" || k == "SWAYSOCK");
        let sway = Desktop::from_env(|k| k == "SWAYSOCK");
        let other = Desktop::from_env(|_| false);
        if cfg!(target_os = "linux") {
            assert_eq!(hypr, Desktop::Hyprland);
            assert_eq!(sway, Desktop::Sway);
        }
        assert_eq!(other, Desktop::Other);
    }
}
