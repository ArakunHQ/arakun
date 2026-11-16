//! What `arakun <command>` does in the window (see `crate::control`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::{App, Context, Window};

use super::Shell;
use crate::control::{self, Reply, Request};
use crate::island::AlertKind;
use crate::model::{Split, SplitDir, Status, TabKind};

impl Shell {
    /// Answers commands from the control socket for as long as the window lives.
    pub(super) fn listen_for_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut incoming) = control::serve() else {
            return;
        };
        self._control = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some((req, done)) = incoming.next().await {
                // Page commands wait on the page, so they answer later, on their own.
                if req.cmd == "dev" {
                    let reply = match crate::dev::handle(&req) {
                        Ok(steps) => cx
                            .update(|window, cx| crate::dev::play(steps, window, cx))
                            .map(|_| Reply::ok(""))
                            .unwrap_or_else(|_| Reply::err("the window is gone")),
                        Err(reply) => reply,
                    };
                    let _ = done.send(reply);
                    continue;
                }
                if req.cmd == "rename" {
                    let _ = this.update(cx, |this, cx| this.rename_command(req, done, cx));
                    continue;
                }
                if req.cmd == "page" {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.page_command(req, done, window, cx)
                    });
                    continue;
                }
                if req.cmd == "run"
                    && let Some(wait) = ReadyWait::parse(&req.args)
                {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.run_and_wait(req, wait, done, window, cx)
                    });
                    continue;
                }
                let reply = this
                    .update_in(cx, |this, window, cx| this.run_command(req, window, cx))
                    .unwrap_or_else(|_| Reply::err("the window is gone"));
                let _ = done.send(reply);
            }
        }));
        if let Some(dir) = control::open_at_start() {
            self.add_or_show(&dir, window, cx);
        }
    }

    fn run_command(&mut self, req: Request, window: &mut Window, cx: &mut Context<Self>) -> Reply {
        let arg = req.args.first().cloned().unwrap_or_default();
        if req.cmd == "add" {
            return self.add_or_show(Path::new(&arg), window, cx);
        }
        if req.cmd == "ls" {
            return Reply::ok(self.listing());
        }
        if req.cmd == "palette" {
            window.activate_window();
            if !matches!(self.modal, Some((super::Modal::Palette(_), _))) {
                self.open_palette(window, cx);
            }
            return Reply::ok("palette open");
        }
        if req.cmd == "agent" {
            return self.agent_command(&req, window, cx);
        }
        #[cfg(feature = "team")]
        if req.cmd == "team" {
            return self.team_command(&req.args, window, cx);
        }
        if req.cmd == "restart" {
            if !cfg!(unix) {
                return Reply::err("arakun restart works on Linux only for now");
            }
            self.restarting = true;
            // Quits a moment later, once this answer is on its way.
            cx.spawn(async move |_, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                cx.update(|cx| cx.quit());
            })
            .detach();
            return Reply::ok("restarting arakun; turns running now go on after it is back");
        }
        let Some(env_path) = self.command_env(&req) else {
            return Reply::err("no project is open in arakun");
        };
        let visible = self.has_projects() && self.env().path == env_path;
        let place = self.env_label(&env_path);
        let where_ = if visible {
            String::new()
        } else {
            format!(" in {place} (not on screen)")
        };
        match req.cmd.as_str() {
            #[cfg(feature = "coderlabs")]
            "canvas" if arg == "--stats" => {
                Reply::ok(self.canvas_view.update(cx, |v, _| v.take_stats()))
            }
            #[cfg(feature = "coderlabs")]
            "canvas" if arg == "--bench" => {
                let num = |i: usize, d: f32| {
                    req.args
                        .get(i)
                        .and_then(|a| a.parse::<f32>().ok())
                        .unwrap_or(d)
                };
                match self
                    .canvas_view
                    .update(cx, |v, cx| v.start_bench(num(1, 0.3), num(2, 2400.), cx))
                {
                    Ok(m) => Reply::ok(m),
                    Err(e) => Reply::err(e),
                }
            }
            #[cfg(feature = "coderlabs")]
            "canvas" => {
                self.open_canvas_tab(Some(arg.clone()), window, cx);
                Reply::ok(format!("opening {arg} in the Canvas tab{where_}"))
            }
            "browse" => {
                let id =
                    self.push_browser_tab(&env_path, arg.clone(), !req.side, false, window, cx);
                if req.side
                    && let Some(id) = id
                {
                    self.show_beside(&env_path, id, window, cx);
                }
                Reply::ok(format!("opened {arg}{where_}"))
            }
            "open" => {
                let path = PathBuf::from(&arg);
                if crate::explorer::file_kind(&path) == crate::explorer::FileKind::External {
                    cx.open_with_system(&path);
                    return Reply::ok(format!("opened {arg} with the default app"));
                }
                let line = req.args.get(1).and_then(|n| n.parse::<usize>().ok());
                let id = self.open_doc_in(&env_path, TabKind::File { path }, !req.side, cx);
                if let (Some(id), Some(line)) = (id, line) {
                    self.reveal_line(&env_path, id, line, cx);
                }
                match id {
                    Some(id) if req.side => self.show_beside(&env_path, id, window, cx),
                    Some(_) => self.save(cx),
                    None => return Reply::err("could not open the file"),
                }
                let at = line.map(|n| format!(" at line {n}")).unwrap_or_default();
                Reply::ok(format!("opened {arg}{at}{where_}"))
            }
            "run" => match self.run_in_terminal(&env_path, &arg, false, window, cx) {
                Some(i) => Reply::ok(format!(
                    "running in terminal {n}{where_}; `arakun logs {n}` shows its output",
                    n = self
                        .find_env(&env_path)
                        .and_then(|e| e.terminals.get(i))
                        .map_or(i + 1, |t| t.number)
                )),
                None => Reply::err("could not start a terminal"),
            },
            "logs" => {
                let lines = arg.parse().unwrap_or(control::LOG_LINES);
                let which = req.args.get(1).map_or("", String::as_str);
                let one = req.args.get(2).is_some_and(|a| a == "one");
                self.terminal_logs(&env_path, which, lines, one, &place, cx)
            }
            // `arakun check --set <command>`: the project's check from now on.
            "check" if !arg.is_empty() => {
                let Some(pi) = self
                    .projects
                    .iter()
                    .position(|p| p.env_index(&env_path).is_some())
                else {
                    return Reply::err("no project here");
                };
                self.projects[pi].check = Some(arg.clone());
                self.projects[pi].check_declined = false;
                self.save(cx);
                Reply::ok(format!("check for {}: {arg}", self.projects[pi].name))
            }
            // `arakun check` runs the command itself; it only needs to know which.
            "check" => match self.project_of_env(&env_path).and_then(|p| p.check.clone()) {
                Some(command) => Reply::ok(format!("{}\n{command}", env_path.display())),
                None => Reply::err(format!(
                    "{place} has no check command; set one with \"Check command…\" in the project's menu"
                )),
            },
            "notify" => {
                self.notify(
                    AlertKind::Info,
                    &format!("Arakun · {place}"),
                    &arg,
                    None,
                    cx,
                );
                Reply::ok("")
            }
            other => Reply::err(format!("unknown command `{other}` (see arakun help)")),
        }
    }

    /// `arakun run --ready/--port`: starts the command like `arakun run`, then
    /// answers once it is up, or with its output when it exits first or the
    /// time runs out.
    fn run_and_wait(
        &mut self,
        req: Request,
        wait: ReadyWait,
        done: oneshot::Sender<Reply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(env_path) = self.command_env(&req) else {
            let _ = done.send(Reply::err("no project is open in arakun"));
            return;
        };
        let command = req.args.first().cloned().unwrap_or_default();
        // Otherwise an old server on the port would pass for the new one.
        if let Some(port) = wait.port
            && port_open(port)
        {
            let _ = done.send(Reply::err(format!(
                "port {port} already answers, before `{command}` started; is a server already running? \
                 `arakun logs` lists the terminals"
            )));
            return;
        }
        let visible = self.has_projects() && self.env().path == env_path;
        let where_ = if visible {
            String::new()
        } else {
            format!(" in {} (not on screen)", self.env_label(&env_path))
        };
        let terminal = self
            .run_in_terminal(&env_path, &command, false, window, cx)
            .and_then(|i| {
                let tab = self.find_env(&env_path)?.terminals.get(i)?;
                Some((tab.number, tab.terminal.downgrade()))
            });
        let Some((n, terminal)) = terminal else {
            let _ = done.send(Reply::err("could not start a terminal"));
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            let start = Instant::now();
            let mut port_up = wait.port.is_none();
            let reply = loop {
                let Ok(out) = terminal.read_with(cx, |t, _| t.recent_text(usize::MAX)) else {
                    break Reply::err(format!("terminal {n} was closed before `{command}` was ready"));
                };
                if !port_up && let Some(port) = wait.port {
                    port_up = cx.background_executor().spawn(async move { port_open(port) }).await;
                }
                let elapsed = start.elapsed();
                match wait.progress(&out, port_up) {
                    Progress::Ready => {
                        break Reply::ok(with_tail(
                            format!(
                                "ready after {:.1} s ({}), running in terminal {n}{where_}; `arakun logs {n}` shows more",
                                elapsed.as_secs_f32(),
                                wait.describe(),
                            ),
                            &out,
                            READY_TAIL,
                        ));
                    }
                    Progress::Exited(code) => {
                        break Reply::err(with_tail(
                            format!(
                                "`{command}` exited with code {code} before it was ready ({})",
                                wait.missing(&out, port_up),
                            ),
                            &out,
                            FAIL_TAIL,
                        ));
                    }
                    Progress::Waiting if elapsed >= wait.timeout => {
                        break Reply::err(with_tail(
                            format!(
                                "not ready after {} s ({}); `{command}` is still running in terminal {n}{where_}",
                                wait.timeout.as_secs(),
                                wait.missing(&out, port_up),
                            ),
                            &out,
                            FAIL_TAIL,
                        ));
                    }
                    Progress::Waiting => cx.background_executor().timer(READY_POLL).await,
                }
            };
            let _ = done.send(reply);
        })
        .detach();
    }

    /// `arakun logs`: the output of the dock terminal `which` (its number, or a
    /// word of its title or command), or the list of them when that is unclear.
    fn terminal_logs(
        &self,
        env_path: &Path,
        which: &str,
        lines: usize,
        one: bool,
        place: &str,
        cx: &App,
    ) -> Reply {
        let terminals = self
            .find_env(env_path)
            .map_or(&[][..], |e| &e.terminals[..]);
        let opened = self.find_env(env_path).map_or(0, |e| e.terminals_opened);
        if terminals.is_empty() {
            return Reply::err(format!("no terminals are open in {place}"));
        }
        let names: Vec<String> = terminals
            .iter()
            .map(|t| {
                let terminal = t.terminal.read(cx);
                let title = terminal.title();
                let Some(cmd) = &t.command else {
                    return title.to_string();
                };
                let state = match run_status(&terminal.recent_text(usize::MAX)) {
                    Some(code) => format!("exited {code}"),
                    None => "running".into(),
                };
                format!("{title} · {cmd}  ({state})")
            })
            .collect();
        let listing = || {
            let mut out = format!("{} terminals in {place}:\n", names.len());
            for (t, name) in terminals.iter().zip(&names) {
                out.push_str(&format!("  {}  {name}\n", t.number));
            }
            out.push_str("`arakun logs <number>` shows one.");
            out
        };
        let pick = if which.is_empty() {
            (names.len() == 1).then_some(0)
        } else if let Ok(n) = which.parse::<usize>() {
            match terminals.iter().position(|t| t.number == n) {
                Some(i) => Some(i),
                None if (1..=opened).contains(&n) => {
                    return Reply::err(format!("terminal {n} was closed\n{}", listing()));
                }
                None => return Reply::err(format!("there is no terminal {n}\n{}", listing())),
            }
        } else {
            let needle = which.to_lowercase();
            let hits: Vec<usize> = (0..names.len())
                .filter(|&i| names[i].to_lowercase().contains(&needle))
                .collect();
            match hits[..] {
                [i] => Some(i),
                [] => return Reply::err(format!("no terminal matches `{which}`\n{}", listing())),
                _ => {
                    return Reply::err(format!(
                        "more than one terminal matches `{which}`\n{}",
                        listing()
                    ));
                }
            }
        };
        match pick {
            Some(i) => Reply::ok(terminals[i].terminal.read(cx).recent_text(lines)),
            // `--until` needs one terminal to watch, not the list.
            None if one => Reply::err(format!("say which terminal to watch\n{}", listing())),
            None => Reply::ok(listing()),
        }
    }

    /// The environment a command acts on: the one its file (for `open`) or its
    /// directory belongs to, else the one on screen.
    pub(super) fn command_env(&self, req: &Request) -> Option<PathBuf> {
        let anchor = match req.cmd.as_str() {
            "open" => req
                .args
                .first()
                .map_or_else(|| req.cwd.clone(), PathBuf::from),
            _ => req.cwd.clone(),
        };
        self.env_for(&anchor)
            .or_else(|| self.env_for(&req.cwd))
            .or_else(|| self.has_projects().then(|| self.env().path.clone()))
    }

    /// The environment a path belongs to: the deepest one containing it.
    pub(super) fn env_for(&self, path: &Path) -> Option<PathBuf> {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .filter(|e| {
                path.starts_with(&e.path)
                    || crate::platform::canonicalize(&e.path)
                        .is_ok_and(|real| path.starts_with(real))
            })
            .max_by_key(|e| e.path.as_os_str().len())
            .map(|e| e.path.clone())
    }

    pub(super) fn env_label(&self, env_path: &Path) -> String {
        self.projects
            .iter()
            .find_map(|p| {
                let e = p.environments.iter().find(|e| e.path == env_path)?;
                Some(if e.name == p.name {
                    p.name.to_string()
                } else {
                    format!("{} / {}", p.name, e.name)
                })
            })
            .unwrap_or_else(|| env_path.display().to_string())
    }

    /// Puts tab `id` in the pane beside the active tab without moving the focus.
    pub(super) fn show_beside(
        &mut self,
        env_path: &Path,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        if env.tabs.get(env.active_tab).is_some_and(|t| t.id == id) {
            return;
        }
        let (dir, ratio, focus_first) = env.split.map_or((SplitDir::Right, 0.5, true), |s| {
            (s.dir, s.ratio, s.focus_first)
        });
        env.split = Some(Split {
            other: id,
            dir,
            ratio,
            focus_first,
        });
        self.ensure_active_live(window, cx);
        self.ensure_active_browser(false, window, cx);
        self.save(cx);
        cx.notify();
    }

    /// `arakun <dir>`: switches to the environment holding `dir`, or adds it as a project.
    fn add_or_show(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) -> Reply {
        if !dir.is_dir() {
            return Reply::err(format!("not a directory: {}", dir.display()));
        }
        let found = self.env_for(dir).and_then(|env_path| {
            self.projects.iter().enumerate().find_map(|(pi, p)| {
                let ei = p.env_index(&env_path)?;
                Some((pi, ei))
            })
        });
        let reply = match found {
            Some((pi, ei)) => {
                self.select_env(pi, ei, window, cx);
                Reply::ok(format!(
                    "showing {}",
                    self.env_label(&self.env().path.clone())
                ))
            }
            None => {
                self.add_project(dir.to_path_buf(), window, cx);
                Reply::ok(format!("added {}", dir.display()))
            }
        };
        window.activate_window();
        reply
    }

    fn listing(&self) -> String {
        let mut out = String::new();
        for (pi, p) in self.projects.iter().enumerate() {
            for (ei, e) in p.environments.iter().enumerate() {
                let shown = pi == self.active_project && ei == p.active_env;
                let count = |st: Status| e.sessions.iter().filter(|s| s.status == st).count();
                let mut state = Vec::new();
                for (n, label) in [
                    (count(Status::Working), "working"),
                    (count(Status::NeedsInput), "needs input"),
                ] {
                    if n > 0 {
                        state.push(format!("{n} {label}"));
                    }
                }
                let sessions = match e.sessions.len() {
                    0 => "no sessions".to_string(),
                    1 => "1 session".to_string(),
                    n => format!("{n} sessions"),
                };
                let state = if state.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", state.join(", "))
                };
                out.push_str(&format!(
                    "{} {}  [{}]  {}  {sessions}{state}\n",
                    if shown { "*" } else { " " },
                    self.env_label(&e.path),
                    e.branch,
                    e.path.display(),
                ));
            }
        }
        if out.is_empty() {
            out.push_str("no projects\n");
        }
        out.trim_end().to_string()
    }
}

const READY_POLL: Duration = Duration::from_millis(250);
/// Output lines shown when the command is up (enough for the URL a dev server
/// prints), and when it is not.
const READY_TAIL: usize = 15;
const FAIL_TAIL: usize = 40;

/// What `arakun run --ready/--port` waits for.
#[derive(Debug, PartialEq)]
struct ReadyWait {
    text: Option<String>,
    port: Option<u16>,
    timeout: Duration,
}

#[derive(Debug, PartialEq)]
enum Progress {
    Ready,
    Exited(i32),
    Waiting,
}

impl ReadyWait {
    /// From `[command, text, port, secs]`; `None` when there is nothing to wait for.
    fn parse(args: &[String]) -> Option<Self> {
        let text = args.get(1).filter(|s| !s.is_empty()).cloned();
        let port = args.get(2).and_then(|p| p.parse().ok()).filter(|p| *p > 0);
        if text.is_none() && port.is_none() {
            return None;
        }
        let secs = args
            .get(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(control::RUN_WAIT);
        Some(Self {
            text,
            port,
            timeout: Duration::from_secs(secs.clamp(1, control::MAX_RUN_WAIT)),
        })
    }

    fn saw_text(&self, out: &str) -> bool {
        self.text
            .as_ref()
            .is_none_or(|t| out.to_lowercase().contains(&t.to_lowercase()))
    }

    /// Ready once every condition holds; a command that ends first has failed,
    /// whatever its code.
    fn progress(&self, out: &str, port_up: bool) -> Progress {
        if port_up && self.saw_text(out) {
            return Progress::Ready;
        }
        match run_status(out) {
            Some(code) => Progress::Exited(code),
            None => Progress::Waiting,
        }
    }

    fn describe(&self) -> String {
        let mut what = Vec::new();
        if let Some(t) = &self.text {
            what.push(format!("output showed \"{t}\""));
        }
        if let Some(p) = self.port {
            what.push(format!("port {p} answers"));
        }
        what.join(", ")
    }

    fn missing(&self, out: &str, port_up: bool) -> String {
        let mut what = Vec::new();
        if let Some(t) = self.text.as_ref().filter(|_| !self.saw_text(out)) {
            what.push(format!("output never showed \"{t}\""));
        }
        if let Some(p) = self.port.filter(|_| !port_up) {
            what.push(format!("port {p} never answered"));
        }
        what.join(", ")
    }
}

/// Something accepts connections on `port` here, over IPv4 or IPv6 (dev
/// servers often listen on only one of them).
pub(super) fn port_open(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
    [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ]
    .iter()
    .any(|addr| TcpStream::connect_timeout(addr, Duration::from_millis(300)).is_ok())
}

fn tail(out: &str, lines: usize) -> String {
    let all: Vec<&str> = out.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// `message`, then the last `lines` of `out` under it when there are any.
fn with_tail(message: String, out: &str, lines: usize) -> String {
    match tail(out, lines) {
        t if t.trim().is_empty() => message,
        t => format!("{message}\n{t}"),
    }
}

/// The code in the last `[exit N]` line `run_in_terminal` prints after its command.
fn run_status(output: &str) -> Option<i32> {
    output
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("[exit ")?.strip_suffix(']')?.parse().ok())
}

/// Whether a `run_in_terminal` command has ended and its terminal still waits
/// for Enter (nobody opened its shell): output nobody is using.
pub(super) fn run_waiting(output: &str) -> bool {
    output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.trim() == crate::platform::RUN_SHELL_HINT)
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_status_reads_the_last_exit_line() {
        assert_eq!(super::run_status("building\n"), None);
        assert_eq!(
            super::run_status("ok\n[exit 0]\nbanner\n$ make\n[exit 2]\nprompt"),
            Some(2)
        );
    }

    #[test]
    fn run_waiting_only_until_a_shell_opens() {
        let hint = crate::platform::RUN_SHELL_HINT;
        assert!(!super::run_waiting("building"));
        assert!(super::run_waiting(&format!("done\n[exit 0]\n{hint}\n\n")));
        assert!(!super::run_waiting(&format!(
            "done\n[exit 0]\n{hint}\nbanner\n$ "
        )));
    }

    #[test]
    fn run_waits_for_its_text_and_port() {
        use super::{Progress, ReadyWait};
        use std::time::Duration;
        let parse =
            |a: &[&str]| ReadyWait::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(parse(&["make"]), None);
        assert_eq!(parse(&["make", "", "", ""]), None);
        let both = parse(&["vite", "local:", "5173", ""]).unwrap();
        assert_eq!(both.timeout, Duration::from_secs(crate::control::RUN_WAIT));
        assert_eq!(
            both.progress("  ➜  Local:   http://localhost:5173/", false),
            Progress::Waiting
        );
        assert_eq!(
            both.progress("  ➜  Local:   http://localhost:5173/", true),
            Progress::Ready
        );
        assert_eq!(both.progress("starting", true), Progress::Waiting);
        assert_eq!(
            both.progress("error: EADDRINUSE\n\n[exit 1]", false),
            Progress::Exited(1)
        );
        assert_eq!(
            both.missing("starting", true),
            "output never showed \"local:\""
        );
        assert_eq!(
            both.describe(),
            "output showed \"local:\", port 5173 answers"
        );
        let text = parse(&["make", "Done", "", "5"]).unwrap();
        assert_eq!(text.timeout, Duration::from_secs(5));
        assert_eq!(text.progress("Done\n\n[exit 0]", true), Progress::Ready);
        assert_eq!(
            text.progress("built\n\n[exit 0]", true),
            Progress::Exited(0)
        );
        assert_eq!(
            text.missing("built\n\n[exit 0]", true),
            "output never showed \"Done\""
        );
        assert_eq!(super::tail("a\nb\nc", 2), "b\nc");
        assert_eq!(super::with_tail("m".into(), "\n\n", 5), "m");
        assert_eq!(super::with_tail("m".into(), "a\nb", 1), "m\nb");
    }
}
