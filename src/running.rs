//! The local servers that run: every process that listens on a TCP port and
//! is either this Arakun's (a dock terminal or an agent started it) or works
//! in the folder of one of its environments. A dev server left behind keeps
//! its memory until someone ends it, so this finds them and can stop one.
//!
//! [`scan`] asks the system what listens and whose process it is; [`servers`]
//! holds the rules, as a function of what the system said, so tests drive it
//! without the system.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// A dock terminal's shell, as Arakun knows it.
#[derive(Clone, Debug)]
pub struct TerminalRoot {
    pub pid: u32,
    pub env: PathBuf,
    pub number: usize,
}

/// What Arakun knows when it looks: its own pid, its terminals, the folders
/// of its local environments.
#[derive(Clone, Debug)]
pub struct Scope {
    pub app: u32,
    pub terminals: Vec<TerminalRoot>,
    pub envs: Vec<PathBuf>,
}

/// Who started a server.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The dock terminal with this number.
    Terminal(usize),
    Agent,
    Outside,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Server {
    /// The process Stop ends, with its descendants.
    pub pid: u32,
    /// When that process started, in unix seconds. A pid is reused once its
    /// process ends, so Stop checks this too.
    pub started: u64,
    /// Sorted, each port once.
    pub ports: Vec<u16>,
    /// A short label, such as "npm run dev".
    pub command: String,
    pub cwd: Option<PathBuf>,
    /// The folder of the environment it belongs to.
    pub env: PathBuf,
    /// Bytes, of `pid` and its descendants.
    pub memory: u64,
    pub source: Source,
}

/// A process, as far as the rules need it.
struct Proc {
    parent: Option<u32>,
    /// Unix seconds; 0 when the system would not say.
    started: u64,
    name: String,
    cmd: Vec<String>,
    cwd: Option<PathBuf>,
    memory: u64,
}

/// The servers that run now. Blocking; the caller runs it off the main thread.
pub fn scan(scope: &Scope) -> Vec<Server> {
    let listeners = listeners();
    if listeners.is_empty() {
        return Vec::new();
    }
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        ProcessRefreshKind::new().with_memory(),
    );
    let mut procs = table(&system);
    // The command line and the folder are read from the other process's own
    // memory, one process at a time, so only the ones a row can show are read.
    let mut shown: Vec<Pid> = Vec::new();
    for &(_, listener) in &listeners {
        shown.push(Pid::from_u32(listener));
        if let Some((below, _)) = below_terminal(&ancestry(listener, &procs), scope) {
            shown.push(Pid::from_u32(below));
        }
    }
    shown.sort_unstable();
    shown.dedup();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&shown),
        ProcessRefreshKind::new()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    for pid in shown {
        let (Some(process), Some(proc)) = (system.process(pid), procs.get_mut(&pid.as_u32()))
        else {
            continue;
        };
        proc.cmd = text_args(process);
        proc.cwd = process.cwd().map(Path::to_path_buf);
    }
    servers(&listeners, &procs, scope)
}

/// Ends `pid` and its descendants if `pid` still started at `started`.
/// Blocking. True when it was found.
pub fn stop(pid: u32, started: u64) -> bool {
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
    let procs = table(&system);
    if pid == 0 || procs.get(&pid).is_none_or(|proc| proc.started != started) {
        return false;
    }
    let family = family(pid, &children(&procs));
    // Arakun does not end itself, whatever it was asked.
    if family.contains(&std::process::id()) {
        return false;
    }
    // Children first: a parent that outlives them only sees them end, while a
    // child that outlives its parent is left with no one to end it.
    for &member in family.iter().rev() {
        if let Some(process) = system.process(Pid::from_u32(member)) {
            end(process);
        }
    }
    true
}

/// Unix asks, so a server can close what it has open; Windows has no such
/// request, so the process is ended.
fn end(process: &sysinfo::Process) {
    #[cfg(unix)]
    if process.kill_with(sysinfo::Signal::Term).is_some() {
        return;
    }
    process.kill();
}

/// The processes of `system`, by pid.
fn table(system: &System) -> HashMap<u32, Proc> {
    system
        .processes()
        .iter()
        // Linux lists every thread as a process of its own, a child of its
        // process: counted, a server's memory would add up many times over.
        .filter(|(_, process)| process.thread_kind().is_none())
        .map(|(pid, process)| {
            let proc = Proc {
                parent: process.parent().map(Pid::as_u32),
                started: process.start_time(),
                name: process.name().to_string_lossy().into_owned(),
                cmd: text_args(process),
                cwd: process.cwd().map(Path::to_path_buf),
                memory: process.memory(),
            };
            (pid.as_u32(), proc)
        })
        .collect()
}

fn text_args(process: &sysinfo::Process) -> Vec<String> {
    process
        .cmd()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// The servers among `listeners` (a port and the pid that listens on it),
/// given every process and what Arakun knows.
fn servers(listeners: &[(u16, u32)], procs: &HashMap<u32, Proc>, scope: &Scope) -> Vec<Server> {
    let children = children(procs);
    // Lowest port first: ports come out sorted, and a command with several
    // listeners takes its folder from the same one every time.
    let mut listeners = listeners.to_vec();
    listeners.sort_unstable();
    // The same port on IPv4 and IPv6 is one port.
    listeners.dedup();
    let mut found: Vec<Server> = Vec::new();
    for (port, listener) in listeners {
        let Some((pid, source, env)) = owner(listener, procs, scope) else {
            continue;
        };
        if let Some(server) = found.iter_mut().find(|server| server.pid == pid) {
            if !server.ports.contains(&port) {
                server.ports.push(port);
            }
            continue;
        }
        let Some(proc) = procs.get(&pid) else {
            continue;
        };
        let listener_cwd = || procs.get(&listener).and_then(|proc| proc.cwd.clone());
        found.push(Server {
            pid,
            started: proc.started,
            ports: vec![port],
            command: command_label(&proc.name, &proc.cmd),
            cwd: proc.cwd.clone().or_else(listener_cwd),
            env,
            memory: family(pid, &children)
                .iter()
                .filter_map(|member| procs.get(member))
                .map(|proc| proc.memory)
                .sum(),
            source,
        });
    }
    found.sort_by(|a, b| (&a.env, a.ports[0], a.pid).cmp(&(&b.env, b.ports[0], b.pid)));
    found
}

/// The process Stop ends for `listener`, who started it and its environment.
/// None when it is not a server to list.
fn owner(
    listener: u32,
    procs: &HashMap<u32, Proc>,
    scope: &Scope,
) -> Option<(u32, Source, PathBuf)> {
    // Arakun and a terminal's shell are not servers, whatever they listen on.
    if listener == scope.app || scope.terminals.iter().any(|shell| shell.pid == listener) {
        return None;
    }
    let chain = ancestry(listener, procs);
    if let Some((below, shell)) = below_terminal(&chain, scope) {
        return Some((below, Source::Terminal(shell.number), shell.env.clone()));
    }
    // What Arakun itself started is an agent or one of its own helpers, not
    // a server: stopping it would end the agent.
    if chain.get(1) == Some(&scope.app) {
        return None;
    }
    // Never an ancestor here: it may be the agent itself.
    let cwd = procs.get(&listener)?.cwd.as_deref()?;
    let env = deepest(&scope.envs, cwd)?.clone();
    let source = if chain[1..].contains(&scope.app) {
        Source::Agent
    } else {
        Source::Outside
    };
    Some((listener, source, env))
}

/// The process right below a terminal's shell in `chain` (a process, then its
/// ancestors), and that terminal: what the terminal ran, so stopping it ends
/// the whole command and never the shell.
fn below_terminal<'a>(chain: &[u32], scope: &'a Scope) -> Option<(u32, &'a TerminalRoot)> {
    chain.windows(2).find_map(|pair| {
        let shell = scope.terminals.iter().find(|shell| shell.pid == pair[1])?;
        Some((pair[0], shell))
    })
}

/// The parent of `pid`, when the link can be trusted.
fn parent_of(pid: u32, procs: &HashMap<u32, Proc>) -> Option<u32> {
    let proc = procs.get(&pid)?;
    let parent = proc.parent.filter(|parent| *parent != pid)?;
    let above = procs.get(&parent)?;
    // A process keeps the pid of its parent after the parent ends, and the
    // system gives that pid to a new process (soon, on Windows). A "parent"
    // that started after its child is that stranger.
    let known = proc.started != 0 && above.started != 0;
    (!known || above.started <= proc.started).then_some(parent)
}

/// `pid`, then its ancestors, nearest first.
fn ancestry(pid: u32, procs: &HashMap<u32, Proc>) -> Vec<u32> {
    let mut chain = vec![pid];
    while let Some(parent) = parent_of(chain[chain.len() - 1], procs) {
        // Reused pids can make a loop.
        if chain.contains(&parent) {
            break;
        }
        chain.push(parent);
    }
    chain
}

/// Each process's children, by the links that can be trusted.
fn children(procs: &HashMap<u32, Proc>) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &pid in procs.keys() {
        if let Some(parent) = parent_of(pid, procs) {
            children.entry(parent).or_default().push(pid);
        }
    }
    children
}

/// `pid`, then its descendants, each after its parent.
fn family(pid: u32, children: &HashMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut family = vec![pid];
    let mut next = 0;
    while next < family.len() {
        for &child in children.get(&family[next]).into_iter().flatten() {
            // Reused pids can make a loop.
            if !family.contains(&child) {
                family.push(child);
            }
        }
        next += 1;
    }
    family
}

/// The folder of `envs` that holds `path` most closely.
fn deepest<'a>(envs: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    let path = folder_parts(path);
    envs.iter()
        .map(|env| (folder_parts(env), env))
        .filter(|(env, _)| path.starts_with(env))
        .max_by_key(|(env, _)| env.len())
        .map(|(_, env)| env)
}

/// The names in `path`, so that two spellings of one folder compare equal:
/// with or without Windows's `\\?\` in front or a separator at the end, and
/// in any case on Windows.
fn folder_parts(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| text.strip_prefix(r"\\?\"))
        .unwrap_or(&text);
    text.split(std::path::is_separator)
        .filter(|part| !part.is_empty())
        .map(|part| {
            if cfg!(windows) {
                part.to_lowercase()
            } else {
                part.to_string()
            }
        })
        .collect()
}

/// A short label for a server's row: what someone would have typed, not the
/// path of the interpreter that runs it.
fn command_label(name: &str, cmd: &[String]) -> String {
    let program = cmd.first().map(|first| without_exe(file_name(first)));
    let Some(program) = program.filter(|program| !program.is_empty()) else {
        return one_line(without_exe(name));
    };
    let rest = &cmd[1..];
    fn words(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }
    let told: Option<Vec<&str>> = match program.to_ascii_lowercase().as_str() {
        // npm, vite, next and the like are scripts that node runs: the script
        // is the command.
        "node" | "bun" | "deno" => rest.split_first().and_then(|(script, args)| {
            let mut told = vec![script_name(script)?];
            told.extend(words(args));
            Some(told)
        }),
        // A shell that only runs one command: that command.
        "cmd" => rest
            .iter()
            .position(|arg| !arg.starts_with('/') || arg.eq_ignore_ascii_case("/c"))
            .filter(|at| rest[*at].eq_ignore_ascii_case("/c"))
            .map(|at| words(&rest[at + 1..])),
        "sh" | "bash" => rest
            .split_first()
            .filter(|(flag, _)| *flag == "-c")
            .map(|(_, command)| words(command)),
        _ => None,
    };
    let told = told.filter(|told| !told.is_empty()).unwrap_or_else(|| {
        let mut told = vec![program];
        told.extend(words(rest));
        told
    });
    one_line(&told.join(" "))
}

/// The name of the script at `arg` when it is a path to one: `npm` for
/// `…/npm/bin/npm-cli.js`. A bare word is a subcommand (`bun run dev`) or a
/// file someone typed (`node server.js`), and stays as it is.
fn script_name(arg: &str) -> Option<&str> {
    if arg.starts_with('-') || !arg.contains(['/', '\\']) {
        return None;
    }
    let file = file_name(arg);
    let stem = match file.rfind('.') {
        Some(dot) if dot > 0 => &file[..dot],
        _ => file,
    };
    let stem = stem.strip_suffix("-cli").unwrap_or(stem);
    (!stem.is_empty()).then_some(stem)
}

/// What follows the last separator, of either kind: a command line read on
/// one system can name a path the way another writes it.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn without_exe(name: &str) -> &str {
    let cut = name.len().saturating_sub(4);
    match name.get(cut..) {
        Some(end) if end.eq_ignore_ascii_case(".exe") => &name[..cut],
        _ => name,
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every TCP port something listens on, with the pid that listens.
#[cfg(windows)]
fn listeners() -> Vec<(u16, u32)> {
    use std::mem::{offset_of, size_of};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID,
    };
    // AF_INET and AF_INET6, written out so the build does not take all of
    // WinSock for two numbers.
    const IPV4: u32 = 2;
    const IPV6: u32 = 23;
    // Both tables are 32-bit words all through, so they are read as words:
    // where the rows start, how long a row is, and where its port and pid are.
    let v4 = [
        offset_of!(MIB_TCPTABLE_OWNER_PID, table),
        size_of::<MIB_TCPROW_OWNER_PID>(),
        offset_of!(MIB_TCPROW_OWNER_PID, dwLocalPort),
        offset_of!(MIB_TCPROW_OWNER_PID, dwOwningPid),
    ];
    let v6 = [
        offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
        size_of::<MIB_TCP6ROW_OWNER_PID>(),
        offset_of!(MIB_TCP6ROW_OWNER_PID, dwLocalPort),
        offset_of!(MIB_TCP6ROW_OWNER_PID, dwOwningPid),
    ];
    let mut found = Vec::new();
    for (family, [rows, row, port, pid]) in [(IPV4, v4), (IPV6, v6)] {
        let table = tcp_table(family);
        let count = table.first().map_or(0, |count| *count as usize);
        for index in 0..count {
            let at = (rows + index * row) / 4;
            let (Some(port), Some(pid)) = (table.get(at + port / 4), table.get(at + pid / 4))
            else {
                break;
            };
            // The port is in the low 16 bits, in network byte order.
            found.push((u16::from_be(*port as u16), *pid));
        }
    }
    found
}

/// The system's table of listening TCP sockets with their pids, for one
/// address family, as 32-bit words. Empty when the system does not give it.
#[cfg(windows)]
fn tcp_table(family: u32) -> Vec<u32> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_LISTENER,
    };
    let mut table = vec![0u32; 2048];
    // The table can grow between the call that says its size and the next.
    for _ in 0..4 {
        let mut size = (table.len() * 4) as u32;
        // SAFETY: `table` is `size` bytes of writable memory, aligned for the
        // table's 32-bit fields, and the call writes no more than `size` bytes.
        let status = unsafe {
            GetExtendedTcpTable(
                table.as_mut_ptr().cast(),
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match status {
            NO_ERROR => return table,
            ERROR_INSUFFICIENT_BUFFER => table = vec![0u32; (size as usize).div_ceil(4) + 256],
            _ => break,
        }
    }
    Vec::new()
}

/// Every TCP port something listens on, with the pid that listens.
#[cfg(target_os = "linux")]
fn listeners() -> Vec<(u16, u32)> {
    let mut sockets: Vec<(u16, u64)> = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(table) {
            sockets.extend(proc_net_listeners(&text));
        }
    }
    if sockets.is_empty() {
        return Vec::new();
    }
    // The tables name a socket by its inode; whose it is shows in the open
    // files of each process.
    let mut owners: HashMap<u64, u32> = HashMap::new();
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for process in processes.flatten() {
        let name = process.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        // Another user's process does not show its open files: it is skipped.
        let Ok(files) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(link) = std::fs::read_link(file.path()) else {
                continue;
            };
            let Some(inode) = link.to_str().and_then(socket_inode) else {
                continue;
            };
            if sockets.iter().any(|(_, socket)| *socket == inode) {
                // Workers share the socket their parent opened. The lowest
                // pid is most often that parent.
                owners
                    .entry(inode)
                    .and_modify(|owner| *owner = (*owner).min(pid))
                    .or_insert(pid);
            }
        }
    }
    sockets
        .iter()
        .filter_map(|(port, inode)| Some((*port, *owners.get(inode)?)))
        .collect()
}

/// Every TCP port something listens on, with the pid that listens.
#[cfg(target_os = "macos")]
fn listeners() -> Vec<(u16, u32)> {
    use std::process::{Command, Stdio};
    // By its full path: an app opened from the Finder has a short PATH.
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    // lsof exits with an error when nothing listens, and what it printed
    // before one still holds, so the exit code is not read.
    match output {
        Ok(output) => lsof_listeners(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

/// No way to ask on this system.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn listeners() -> Vec<(u16, u32)> {
    Vec::new()
}

/// The listening sockets in the text of `/proc/net/tcp` or `/proc/net/tcp6`:
/// each one's port and inode.
#[cfg(any(target_os = "linux", test))]
fn proc_net_listeners(text: &str) -> Vec<(u16, u64)> {
    text.lines()
        .filter_map(|line| {
            // sl, local address, remote address, state, queues, timer,
            // retransmits, uid, timeout, inode.
            let mut fields = line.split_whitespace();
            let local = fields.nth(1)?;
            let state = fields.nth(1)?;
            let inode = fields.nth(5)?.parse::<u64>().ok()?;
            let port = u16::from_str_radix(local.rsplit_once(':')?.1, 16).ok()?;
            // 0A is LISTEN. A socket with no inode belongs to no process.
            (state == "0A" && inode != 0).then_some((port, inode))
        })
        .collect()
}

/// The inode in what a `/proc/<pid>/fd` link to a socket points at.
#[cfg(any(target_os = "linux", test))]
fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// The listening sockets in what `lsof -Fpn` prints: a `p` line with a pid,
/// then an `n` line for each address that process listens on.
#[cfg(any(target_os = "macos", test))]
fn lsof_listeners(text: &str) -> Vec<(u16, u32)> {
    let mut found = Vec::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(number) = line.strip_prefix('p') {
            pid = number.parse::<u32>().ok();
        } else if let (Some(pid), Some(address)) = (pid, line.strip_prefix('n')) {
            let port = address
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse().ok());
            if let Some(port) = port {
                found.push((port, pid));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arakun is 100, its terminal 2 runs the shell 200 in /work/app.
    fn scope() -> Scope {
        Scope {
            app: 100,
            terminals: vec![TerminalRoot {
                pid: 200,
                env: "/work/app".into(),
                number: 2,
            }],
            envs: vec!["/work/app".into(), "/work/site".into()],
        }
    }

    fn proc(parent: Option<u32>, started: u64) -> Proc {
        Proc {
            parent,
            started,
            name: "node".into(),
            cmd: Vec::new(),
            cwd: None,
            memory: 0,
        }
    }

    fn in_folder(mut proc: Proc, cwd: &str) -> Proc {
        proc.cwd = Some(cwd.into());
        proc
    }

    fn args(cmd: &[&str]) -> Vec<String> {
        cmd.iter().map(|arg| arg.to_string()).collect()
    }

    /// Arakun and its terminal's shell, to add processes to.
    fn procs() -> HashMap<u32, Proc> {
        HashMap::from([(100, proc(None, 10)), (200, proc(Some(100), 20))])
    }

    #[test]
    fn a_parent_that_started_after_its_child_is_not_its_parent() {
        let mut procs = procs();
        // 300 says its parent is 200, but 200 started later: the pid was reused.
        procs.insert(300, in_folder(proc(Some(200), 15), "/work/site"));
        let found = servers(&[(3000, 300)], &procs, &scope());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, Source::Outside);
        assert_eq!(found[0].env, PathBuf::from("/work/site"));

        // A real child of the shell, started in the same second as it.
        procs.insert(300, proc(Some(200), 20));
        let found = servers(&[(3000, 300)], &procs, &scope());
        assert_eq!(found[0].source, Source::Terminal(2));
    }

    #[test]
    fn a_loop_of_parents_ends() {
        let mut procs = procs();
        procs.insert(300, in_folder(proc(Some(301), 30), "/work/app"));
        procs.insert(301, proc(Some(300), 30));
        procs.get_mut(&300).unwrap().memory = 5;
        procs.get_mut(&301).unwrap().memory = 7;
        let found = servers(&[(3000, 300)], &procs, &scope());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, Source::Outside);
        assert_eq!(found[0].memory, 12);
    }

    #[test]
    fn a_listener_under_a_terminal_stops_at_the_process_below_the_shell() {
        let mut procs = procs();
        let mut npm = proc(Some(200), 30);
        npm.cmd = args(&[
            "node",
            "/usr/lib/node_modules/npm/bin/npm-cli.js",
            "run",
            "dev",
        ]);
        procs.insert(300, npm);
        procs.insert(301, proc(Some(300), 31));
        // The folder is outside every environment: the terminal says whose it is.
        procs.insert(302, in_folder(proc(Some(301), 32), "/elsewhere"));
        let found = servers(&[(5173, 302)], &procs, &scope());
        assert_eq!(
            found,
            vec![Server {
                pid: 300,
                started: 30,
                ports: vec![5173],
                command: "npm run dev".into(),
                cwd: Some("/elsewhere".into()),
                env: "/work/app".into(),
                memory: 0,
                source: Source::Terminal(2),
            }]
        );
    }

    #[test]
    fn a_listener_under_an_agent_is_stopped_alone() {
        let mut procs = procs();
        // 400 is an agent, 401 the shell it ran, 402 the server.
        procs.insert(400, in_folder(proc(Some(100), 30), "/work/site"));
        procs.insert(401, in_folder(proc(Some(400), 31), "/work/site"));
        procs.insert(402, in_folder(proc(Some(401), 32), "/work/site/web"));
        let found = servers(&[(3000, 402)], &procs, &scope());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 402);
        assert_eq!(found[0].source, Source::Agent);
        assert_eq!(found[0].env, PathBuf::from("/work/site"));

        // Outside every environment, or in a folder nobody knows: not listed.
        procs.insert(402, in_folder(proc(Some(401), 32), "/tmp"));
        assert_eq!(servers(&[(3000, 402)], &procs, &scope()), Vec::new());
        procs.insert(402, proc(Some(401), 32));
        assert_eq!(servers(&[(3000, 402)], &procs, &scope()), Vec::new());
    }

    #[test]
    fn an_agent_that_listens_is_not_a_server() {
        let mut procs = procs();
        // 400 is what Arakun started: Stop on it would end the agent.
        procs.insert(400, in_folder(proc(Some(100), 30), "/work/site"));
        assert_eq!(servers(&[(4000, 400)], &procs, &scope()), Vec::new());
    }

    #[test]
    fn a_listener_outside_arakun_is_kept_only_inside_an_environment() {
        let mut procs = procs();
        procs.insert(1, proc(None, 1));
        procs.insert(500, in_folder(proc(Some(1), 40), "/work/app/api"));
        procs.insert(501, in_folder(proc(Some(1), 41), "/home/someone"));
        procs.insert(502, proc(Some(1), 42));
        let found = servers(&[(8000, 500), (8001, 501), (8002, 502)], &procs, &scope());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 500);
        assert_eq!(found[0].source, Source::Outside);
        assert_eq!(found[0].env, PathBuf::from("/work/app"));
    }

    #[test]
    fn arakun_and_a_terminals_shell_are_never_listed() {
        let mut procs = procs();
        procs.insert(100, in_folder(proc(None, 10), "/work/app"));
        procs.insert(200, in_folder(proc(Some(100), 20), "/work/app"));
        let found = servers(&[(7000, 100), (7001, 200)], &procs, &scope());
        assert_eq!(found, Vec::new());
    }

    #[test]
    fn the_ports_of_one_command_make_one_server() {
        let mut procs = procs();
        procs.insert(300, proc(Some(200), 30));
        procs.insert(301, in_folder(proc(Some(300), 31), "/work/app/web"));
        procs.insert(302, in_folder(proc(Some(300), 31), "/work/app/api"));
        // 3000 twice: once for IPv4 and once for IPv6.
        let listeners = [(3001, 301), (3000, 302), (3000, 302), (24678, 302)];
        let found = servers(&listeners, &procs, &scope());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 300);
        assert_eq!(found[0].ports, vec![3000, 3001, 24678]);
        // The folder of the listener with the lowest port.
        assert_eq!(found[0].cwd, Some(PathBuf::from("/work/app/api")));
    }

    #[test]
    fn memory_counts_the_process_and_all_it_started() {
        let mut procs = procs();
        for (pid, parent, started, memory) in [
            (400, 100, 30, 1),
            (401, 400, 31, 10),
            (402, 401, 32, 100),
            (403, 402, 33, 1000),
            // Says 401 is its parent, but is older than it: someone else's.
            (404, 401, 5, 10000),
        ] {
            let mut proc = in_folder(proc(Some(parent), started), "/work/app");
            proc.memory = memory;
            procs.insert(pid, proc);
        }
        procs.get_mut(&401).unwrap().cwd = Some("/work/app/web".into());
        let found = servers(&[(3000, 401)], &procs, &scope());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].memory, 1110);
        assert_eq!(found[0].started, 31);
        assert_eq!(found[0].cwd, Some(PathBuf::from("/work/app/web")));
    }

    #[test]
    fn a_folder_holds_what_is_under_it_however_it_is_spelled() {
        let envs = [
            PathBuf::from("/work/app"),
            PathBuf::from("/work/app/nested/"),
        ];
        let inside = |path: &str| deepest(&envs, Path::new(path)).cloned();
        assert_eq!(inside("/work/app"), Some(envs[0].clone()));
        assert_eq!(inside("/work/app/"), Some(envs[0].clone()));
        assert_eq!(inside("/work/app/web/src"), Some(envs[0].clone()));
        // The deepest environment that holds it.
        assert_eq!(inside("/work/app/nested"), Some(envs[1].clone()));
        assert_eq!(inside("/work/app/nested/web"), Some(envs[1].clone()));
        // A name that only starts the same is another folder.
        assert_eq!(inside("/work/app-two"), None);
        assert_eq!(inside("/work"), None);
        // Case only matters where the file system says so.
        let other_case = inside("/Work/APP/web");
        if cfg!(windows) {
            assert_eq!(other_case, Some(envs[0].clone()));
            let envs = [PathBuf::from(r"\\?\C:\Work\App")];
            let found = deepest(&envs, Path::new(r"c:\work\app\web\"));
            assert_eq!(found, Some(&envs[0]));
            let envs = [PathBuf::from(r"C:\work\app\")];
            let found = deepest(&envs, Path::new(r"\\?\C:\work\app"));
            assert_eq!(found, Some(&envs[0]));
        } else {
            assert_eq!(other_case, None);
        }
    }

    #[test]
    fn servers_come_out_by_environment_then_lowest_port() {
        let mut procs = procs();
        procs.insert(1, proc(None, 1));
        procs.insert(500, in_folder(proc(Some(1), 40), "/work/site"));
        procs.insert(501, in_folder(proc(Some(1), 41), "/work/app"));
        procs.insert(502, in_folder(proc(Some(1), 42), "/work/app"));
        procs.insert(503, in_folder(proc(Some(1), 43), "/work/site"));
        let listeners = [
            (9000, 500),
            (8080, 501),
            (9100, 502),
            (3000, 502),
            (4000, 503),
        ];
        let found = servers(&listeners, &procs, &scope());
        let order: Vec<(u32, u16)> = found.iter().map(|s| (s.pid, s.ports[0])).collect();
        assert_eq!(
            order,
            vec![(502, 3000), (501, 8080), (503, 4000), (500, 9000)]
        );
    }

    #[test]
    fn a_label_says_what_was_typed() {
        let label = |cmd: &[&str]| command_label("fallback.exe", &args(cmd));
        assert_eq!(
            label(&[
                r"C:\Program Files\nodejs\node.exe",
                r"C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js",
                "run",
                "dev",
            ]),
            "npm run dev"
        );
        assert_eq!(label(&["node", "/work/app/node_modules/.bin/vite"]), "vite");
        assert_eq!(
            label(&[
                "/usr/bin/node",
                "/work/app/node_modules/next/dist/bin/next",
                "dev",
            ]),
            "next dev"
        );
        // A bare word after the program is not a script's path.
        assert_eq!(label(&["bun", "run", "dev"]), "bun run dev");
        assert_eq!(label(&["node", "server.js"]), "node server.js");
        assert_eq!(
            label(&["cmd.exe", "/c", "npm", "run", "dev"]),
            "npm run dev"
        );
        assert_eq!(
            label(&["CMD.EXE", "/d", "/s", "/c", "npm run dev"]),
            "npm run dev"
        );
        assert_eq!(label(&["sh", "-c", "npm run dev"]), "npm run dev");
        assert_eq!(
            label(&["/bin/bash", "-c", "vite --port 4000"]),
            "vite --port 4000"
        );
        assert_eq!(label(&["bash", "serve.sh"]), "bash serve.sh");
        assert_eq!(
            label(&["/usr/bin/python3", "-m", "http.server", "8000"]),
            "python3 -m http.server 8000"
        );
        assert_eq!(label(&[]), "fallback");
        assert_eq!(command_label("caddy", &[]), "caddy");
        // One line, whatever the command line holds.
        assert_eq!(
            label(&["sh", "-c", "npm run dev\n  --host"]),
            "npm run dev --host"
        );
    }

    #[test]
    fn the_listening_sockets_of_proc_net_are_read() {
        let tcp = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 48291 1 0000000000000000 100 0 0 10 0
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 0 1 0000000000000000 100 0 0 10 0
   2: 0100007F:C350 0100007F:0BB8 01 00000000:00000000 00:00000000 00000000  1000        0 51234 1 0000000000000000 20 4 30 10 -1
";
        assert_eq!(proc_net_listeners(tcp), vec![(3000, 48291)]);
        let tcp6 = "\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1435 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 77001 1 0000000000000000 100 0 0 10 0
";
        assert_eq!(proc_net_listeners(tcp6), vec![(5173, 77001)]);
        assert_eq!(proc_net_listeners(""), Vec::new());

        assert_eq!(socket_inode("socket:[48291]"), Some(48291));
        assert_eq!(socket_inode("pipe:[48291]"), None);
        assert_eq!(socket_inode("/dev/null"), None);
    }

    #[test]
    fn the_listening_sockets_of_lsof_are_read() {
        let text = "p412\nf23\nn*:3000\nf24\nn127.0.0.1:5173\np980\nf7\nn[::1]:8080\nf8\nn*:*\n";
        assert_eq!(
            lsof_listeners(text),
            vec![(3000, 412), (5173, 412), (8080, 980)]
        );
        // An address before any process is nobody's.
        assert_eq!(lsof_listeners("n*:3000\n"), Vec::new());
        assert_eq!(lsof_listeners(""), Vec::new());
    }

    /// The one test that asks the system: the port this test opens is found
    /// as this process's.
    #[test]
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn a_port_this_process_listens_on_is_found() {
        let socket = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        assert!(listeners().contains(&(port, std::process::id())));
    }
}
