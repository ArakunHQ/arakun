//! One host and one human-reviewed command per invocation. Discovery does not
//! authenticate. Login and commands use only the key confirmed by the UI.
//!
//! Linux and macOS only: it runs the system OpenSSH with a process group per run.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use sha2::{Digest as _, Sha256};

const SCAN: &str = "/usr/bin/ssh-keyscan";
const SSH: &str = "/usr/bin/ssh";
const SCAN_LIMIT: usize = 8192;
const OUTPUT_LIMIT: usize = 65536;
const RUN_TIMEOUT: Duration = Duration::from_secs(600);
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);
const TICK: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Target {
    user: String,
    host: String,
    port: u16,
}

impl Target {
    pub fn parse(destination: &str, port: &str) -> Result<Self, String> {
        let (user, host) = destination.split_once('@').ok_or("Use user@host")?;
        if user.is_empty()
            || user.len() > 64
            || !(user.as_bytes()[0].is_ascii_alphanumeric() || user.starts_with('_'))
            || !user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("Use a literal SSH user name".into());
        }
        let bad_host = "Use one literal host name or IP address";
        let ip = if let Some(host) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            Some(IpAddr::V6(host.parse::<Ipv6Addr>().map_err(|_| bad_host)?))
        } else {
            host.parse::<IpAddr>().ok()
        };
        let host = if let Some(ip) = ip {
            let addr = ip.to_canonical();
            if addr.is_unspecified() || addr.is_multicast() || addr == Ipv4Addr::BROADCAST {
                return Err(bad_host.into());
            }
            ip.to_string()
        } else {
            let name = host.strip_suffix('.').unwrap_or(host);
            if name.len() > 253
                || !name.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
                // Some resolvers interpret abbreviated, octal and hex numbers as IPv4.
                || name.split('.').all(|s| {
                    s.bytes().all(|b| b.is_ascii_digit())
                        || s.strip_prefix("0x")
                            .or_else(|| s.strip_prefix("0X"))
                            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_hexdigit()))
                })
            {
                return Err(bad_host.into());
            }
            host.to_ascii_lowercase()
        };
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|&p| p != 0 && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("Use a port from 1 to 65535")?;
        Ok(Self {
            user: user.to_owned(),
            host,
            port,
        })
    }

    pub fn label(&self) -> String {
        if self.host.contains(':') {
            format!("{}@[{}]:{}", self.user, self.host, self.port)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }

    /// The host as `ssh-keyscan` prints it and a known-hosts line names it.
    fn host_field(&self) -> String {
        if self.port == 22 {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        }
    }
}

/// A scanned key, bound to the target it was read from.
#[derive(Clone)]
pub(super) struct HostKey {
    pub fingerprint: String,
    target: Target,
    blob: String,
}

pub(super) struct Output {
    pub text: String,
    pub success: bool,
}

pub(super) fn validate_command(command: &str) -> Result<(), String> {
    if command.len() > 4096
        || command.trim().is_empty()
        || !command.bytes().all(|b| (b' '..=b'~').contains(&b))
    {
        return Err("Use one nonempty line of printable ASCII, up to 4096 characters".into());
    }
    Ok(())
}

/// Reads the target's ED25519 host key without logging in.
pub(super) fn probe(target: &Target, cancel: &AtomicBool) -> Result<HostKey, String> {
    let capture = execute(scan_command(target), cancel, CHECK_TIMEOUT, SCAN_LIMIT)?;
    if !capture.status.success() {
        return Err(format!(
            "SSH host key lookup failed: {}",
            display(&capture.stderr)
        ));
    }
    scanned_key(target, &capture.stdout)
}

pub(super) fn run(
    target: &Target,
    key: &HostKey,
    command: &str,
    cancel: &AtomicBool,
) -> Result<Output, String> {
    run_for(target, key, command, cancel, RUN_TIMEOUT)
}

/// Logs in with the pinned key and runs nothing but `true`.
pub(super) fn check(target: &Target, key: &HostKey, cancel: &AtomicBool) -> Result<(), String> {
    let output = run_for(target, key, "true", cancel, CHECK_TIMEOUT)?;
    if !output.success {
        return Err(format!("SSH login check failed: {}", output.text));
    }
    Ok(())
}

fn scan_command(target: &Target) -> Command {
    let mut cmd = Command::new(SCAN);
    cmd.env_clear().env("LC_ALL", "C").args([
        "-T",
        "5",
        "-t",
        "ed25519",
        "-p",
        &target.port.to_string(),
        "--",
        &target.host,
    ]);
    cmd
}

fn scanned_key(target: &Target, stdout: &[u8]) -> Result<HostKey, String> {
    let invalid = "Expected one ED25519 host key for the selected host and port";
    let mut lines = std::str::from_utf8(stdout)
        .map_err(|_| invalid)?
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let mut fields = lines.next().ok_or(invalid)?.split_ascii_whitespace();
    let host = target.host_field();
    if fields.next() != Some(host.as_str()) || fields.next() != Some("ssh-ed25519") {
        return Err(invalid.into());
    }
    let blob = fields.next().ok_or(invalid)?;
    let raw = STANDARD.decode(blob).map_err(|_| invalid)?;
    // RFC 8709: two SSH strings, the algorithm name and the 32-byte public key.
    if raw.len() != 51
        || !raw.starts_with(b"\0\0\0\x0bssh-ed25519\0\0\0\x20")
        || fields.next().is_some()
        || lines.next().is_some()
    {
        return Err(invalid.into());
    }
    Ok(HostKey {
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&raw))),
        target: target.clone(),
        blob: STANDARD.encode(raw),
    })
}

/// A one-line known-hosts file for one run, removed when dropped.
struct Pin(PathBuf);

impl Pin {
    fn create(target: &Target, key: &HostKey) -> Result<Self, String> {
        if *target != key.target {
            return Err("The confirmed SSH key belongs to a different target".into());
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| format!("Couldn't create SSH pin: {e}"))?;
        // Do not interpolate TMPDIR into an OpenSSH option: it expands tokens.
        let path = PathBuf::from(format!("/tmp/arakun-ssh-{}", URL_SAFE_NO_PAD.encode(nonce)));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("Couldn't create SSH pin: {e}"))?;
        let pin = Self(path);
        writeln!(file, "{} ssh-ed25519 {}", target.host_field(), key.blob)
            .map_err(|e| format!("Couldn't write SSH pin: {e}"))?;
        Ok(pin)
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn ssh_command(target: &Target, pin: &Pin, command: &str) -> Command {
    let mut cmd = Command::new(SSH);
    cmd.current_dir("/")
        .env_clear()
        .env("LC_ALL", "C")
        .env("SSH_ASKPASS_REQUIRE", "never");
    // Authentication can use an already-unlocked agent, never identity files.
    if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK") {
        cmd.env("SSH_AUTH_SOCK", socket);
    }
    cmd.args(["-F", "/dev/null", "-a", "-x", "-n", "-T", "-S", "none"]);
    for option in [
        "BatchMode=yes",
        "PreferredAuthentications=publickey",
        "IdentityFile=none",
        "AddKeysToAgent=no",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "StrictHostKeyChecking=yes",
        "HostKeyAlgorithms=ssh-ed25519",
        "GlobalKnownHostsFile=/dev/null",
        "VerifyHostKeyDNS=no",
        "UpdateHostKeys=no",
        "CheckHostIP=no",
        "NoHostAuthenticationForLocalhost=no",
        "ClearAllForwardings=yes",
        "ControlMaster=no",
        "ControlPersist=no",
        "ProxyCommand=none",
        "PermitLocalCommand=no",
        "CanonicalizeHostname=no",
        "ConnectTimeout=5",
        "ConnectionAttempts=1",
        "EscapeChar=none",
    ] {
        cmd.args(["-o", option]);
    }
    cmd.args(["-o", &format!("UserKnownHostsFile={}", pin.0.display())]);
    cmd.args(["-o", &format!("HostKeyAlias={}", target.host_field())]);
    cmd.args([
        "-l",
        &target.user,
        "-p",
        &target.port.to_string(),
        "--",
        &target.host,
        command,
    ]);
    cmd
}

fn run_for(
    target: &Target,
    key: &HostKey,
    command: &str,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<Output, String> {
    validate_command(command)?;
    not_cancelled(cancel)?;
    let pin = Pin::create(target, key)?;
    let capture = execute(
        ssh_command(target, &pin, command),
        cancel,
        timeout,
        OUTPUT_LIMIT,
    )?;
    command_output(capture)
}

fn command_output(capture: Capture) -> Result<Output, String> {
    let mut bytes = capture.stdout;
    if !bytes.is_empty() && !bytes.ends_with(b"\n") && !capture.stderr.is_empty() {
        bytes.push(b'\n');
    }
    bytes.extend(capture.stderr);
    let text = display(&bytes);
    if capture.status.code() == Some(255) {
        // OpenSSH uses 255 for a changed host key or a failed login too, and
        // the exit code can't tell those from a command that exited 255. Fail
        // closed: end the connection, but keep the output readable.
        return Err(format!(
            "SSH, or the remote command, exited with status 255. \
             Connect again to check the host key. Output: {text}"
        ));
    }
    if capture.status.code().is_none() {
        return Err("SSH was interrupted".into());
    }
    Ok(Output {
        text,
        success: capture.status.success(),
    })
}

/// Remote bytes as text that can't reorder or hide what the panel shows.
fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            c if c.is_control() => '\u{fffd}',
            _ => c,
        })
        .collect()
}

#[derive(Debug)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: ExitStatus,
}

fn not_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("SSH session disconnected".into());
    }
    Ok(())
}

fn execute(
    mut cmd: Command,
    cancel: &AtomicBool,
    timeout: Duration,
    limit: usize,
) -> Result<Capture, String> {
    not_cancelled(cancel)?;
    let deadline = Instant::now() + timeout;
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Couldn't start {}: {e}",
                cmd.get_program().to_string_lossy()
            )
        })?;
    let result = collect(&mut child, cancel, deadline, limit);
    if result.is_err() {
        // collect never reaps on error, so the group leader's PID is still
        // reserved and can't name another process group yet.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let fd = fd.as_raw_fd();
    // The pipe remains owned by the caller, with a single reader.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn chunk(
    reader: &mut impl Read,
    out: &mut Vec<u8>,
    remaining: &mut usize,
) -> Result<Option<usize>, String> {
    let mut buf = [0u8; 4096];
    match reader.read(&mut buf) {
        Ok(n) if n > *remaining => Err("SSH output limit exceeded; disconnected".into()),
        Ok(n) => {
            *remaining -= n;
            out.extend_from_slice(&buf[..n]);
            Ok(Some(n))
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(format!("Couldn't read SSH output: {e}")),
    }
}

fn collect(
    child: &mut Child,
    cancel: &AtomicBool,
    deadline: Instant,
    mut remaining: usize,
) -> Result<Capture, String> {
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    nonblocking(&stdout)
        .and_then(|()| nonblocking(&stderr))
        .map_err(|e| format!("Couldn't watch SSH output: {e}"))?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        not_cancelled(cancel)?;
        if Instant::now() >= deadline {
            return Err("SSH timed out; disconnected".into());
        }
        let a = chunk(&mut stdout, &mut out, &mut remaining)?;
        let b = chunk(&mut stderr, &mut err, &mut remaining)?;
        // Descendants can still hold the pipes. Do not release the group
        // leader's PID until the last writer closes both.
        if a == Some(0)
            && b == Some(0)
            && let Some(status) = child
                .try_wait()
                .map_err(|e| format!("Couldn't wait for SSH: {e}"))?
        {
            return Ok(Capture {
                stdout: out,
                stderr: err,
                status,
            });
        }
        if a.unwrap_or(0) == 0 && b.unwrap_or(0) == 0 {
            std::thread::sleep(TICK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    // An SSH wire-format fixture, not a real identity or a discovered key.
    const BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f";

    fn target() -> Target {
        Target::parse("dev@node.example", "2222").unwrap()
    }

    fn record() -> String {
        format!("[node.example]:2222 ssh-ed25519 {BLOB}\n")
    }

    fn args(cmd: &Command) -> Vec<&str> {
        cmd.get_args().map(|s| s.to_str().unwrap()).collect()
    }

    #[test]
    fn requires_a_single_literal_user_host_and_positive_numeric_port() {
        for invalid in [
            "",
            "host.example",
            "@node.example",
            "dev@",
            "dev@a@b",
            "dev@*.example",
            "dev@host,other",
            "dev@192.0.2.0/24",
            "dev@-host",
            "-user@host",
            "dev@a..b",
            "dev@host:22",
            "dev@host name",
            "dev@host\n",
            "dev@nonascií",
            "dev@127.1",
            "dev@0.0.0.0",
            "dev@[::]",
            "dev@ff02::1",
            "dev@255.255.255.255",
            "dev@[192.0.2.1]",
            "dev@[::1",
            "dev@::1%eth0",
        ] {
            assert!(Target::parse(invalid, "22").is_err(), "{invalid:?}");
        }
        for port in ["", "0", "-1", "+22", "22 ", " 22", "65536", "2_2"] {
            assert!(Target::parse("dev@node.example", port).is_err());
        }
        assert_eq!(
            Target::parse("dev@Node.Example", "65535").unwrap().label(),
            "dev@node.example:65535"
        );
        assert_eq!(
            Target::parse("Dev_2@192.0.2.1", "1").unwrap().label(),
            "Dev_2@192.0.2.1:1"
        );
        assert_eq!(
            Target::parse("dev@[2001:db8::1]", "22").unwrap().label(),
            "dev@[2001:db8::1]:22"
        );
    }

    #[test]
    fn review_accepts_only_bounded_visible_ascii_without_editing_it() {
        assert!(validate_command(" printf '%s\\n' 'hello' ").is_ok());
        assert!(validate_command(&"a".repeat(4096)).is_ok());
        assert!(validate_command(&"a".repeat(4097)).is_err());
        for invalid in [
            "",
            "   ",
            "a\nb",
            "a\rb",
            "x\t",
            "x\0",
            "x\x1b",
            "x\x7f",
            "x\u{202e}",
            "café",
        ] {
            assert!(validate_command(invalid).is_err());
        }
    }

    #[test]
    fn discovery_only_requests_one_host_and_one_key_type() {
        let target = target();
        let cmd = scan_command(&target);
        assert_eq!(cmd.get_program(), SCAN);
        assert_eq!(
            args(&cmd),
            [
                "-T",
                "5",
                "-t",
                "ed25519",
                "-p",
                "2222",
                "--",
                "node.example"
            ]
        );
        assert_eq!(cmd.get_envs().count(), 1);
        let key = scanned_key(&target, record().as_bytes()).unwrap();
        assert_eq!(
            key.fingerprint,
            "SHA256:ZkAslGjFiUHdGf/WUL8rQvkib4PTvQatUV0OUQSncCA"
        );
        let v6 = Target::parse("dev@[2001:db8::8]", "22").unwrap();
        assert!(scanned_key(&v6, format!("2001:db8::8 ssh-ed25519 {BLOB}\n").as_bytes()).is_ok());
    }

    #[test]
    fn only_the_exact_host_port_algorithm_and_wire_key_are_accepted() {
        let line = record();
        for invalid in [
            String::new(),
            line.replace("node.example", "other.example"),
            line.replace("2222", "22"),
            line.replace("ssh-ed25519", "ssh-rsa"),
            line.replace(BLOB, "bm90IGEga2V5"),
            line.replace(BLOB, "not-base64"),
            line.replace(BLOB, &STANDARD.encode([0; 51])),
            format!("{} extra\n", line.trim_end()),
            format!("{line}{line}"),
        ] {
            assert!(scanned_key(&target(), invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn confirmation_is_private_ephemeral_and_bound_to_the_full_target() {
        let target = target();
        let key = scanned_key(&target, record().as_bytes()).unwrap();
        let mut raw = STANDARD.decode(BLOB).unwrap();
        raw[50] ^= 1;
        let changed = record().replace(BLOB, &STANDARD.encode(raw));
        assert_ne!(
            scanned_key(&target, changed.as_bytes())
                .unwrap()
                .fingerprint,
            key.fingerprint
        );
        for other in [
            Target::parse("other@node.example", "2222").unwrap(),
            Target::parse("dev@other.example", "2222").unwrap(),
            Target::parse("dev@node.example", "22").unwrap(),
        ] {
            assert!(Pin::create(&other, &key).is_err());
        }
        let pin = Pin::create(&target, &key).unwrap();
        let path = pin.0.clone();
        assert_eq!(fs::read_to_string(&path).unwrap(), record());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(pin);
        assert!(!path.exists());
    }

    #[test]
    fn ssh_has_one_verbatim_command_after_the_pinned_destination() {
        let target = target();
        let key = scanned_key(&target, record().as_bytes()).unwrap();
        let pin = Pin::create(&target, &key).unwrap();
        let reviewed = "printf '%s' 'hello there'";
        let cmd = ssh_command(&target, &pin, reviewed);
        let args = args(&cmd);
        assert_eq!(cmd.get_program(), SSH);
        assert!(cmd.get_envs().all(|(k, _)| matches!(
            k.to_str(),
            Some("LC_ALL" | "SSH_AUTH_SOCK" | "SSH_ASKPASS_REQUIRE")
        )));
        assert!(args.starts_with(&["-F", "/dev/null", "-a", "-x", "-n", "-T", "-S", "none"]));
        assert!(args.ends_with(&["-l", "dev", "-p", "2222", "--", "node.example", reviewed]));
        for required in [
            format!("UserKnownHostsFile={}", pin.0.display()).as_str(),
            "HostKeyAlias=[node.example]:2222",
            "GlobalKnownHostsFile=/dev/null",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "VerifyHostKeyDNS=no",
            "HostKeyAlgorithms=ssh-ed25519",
            "BatchMode=yes",
            "IdentityFile=none",
            "ClearAllForwardings=yes",
            "ProxyCommand=none",
            "PermitLocalCommand=no",
            "ControlMaster=no",
        ] {
            assert!(
                args.windows(2).any(|pair| pair == ["-o", required]),
                "{required}"
            );
        }
    }

    // Only fixed local fixtures reach the process runner. No SSH executable,
    // agent socket, identity, or real host is used in these tests.
    fn fixture(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.env_clear().args(["-c", script]);
        cmd
    }

    #[test]
    fn both_output_streams_share_one_cap_and_preserve_the_exit_status() {
        let cancel = AtomicBool::new(false);
        let timeout = Duration::from_secs(3);
        let script = "printf hello; printf notice >&2; exit 7";
        let capture = execute(fixture(script), &cancel, timeout, 11).unwrap();
        let out = command_output(capture).unwrap();
        assert_eq!(out.text, "hello\nnotice");
        assert!(!out.success);
        assert!(
            execute(fixture(script), &cancel, timeout, 10)
                .unwrap_err()
                .contains("output limit")
        );
        let capture = execute(
            fixture("printf changed >&2; exit 255"),
            &cancel,
            timeout,
            16,
        )
        .unwrap();
        // 255 ends the connection, but what it printed stays readable.
        let Err(err) = command_output(capture) else {
            panic!("status 255 must end the connection");
        };
        assert!(err.contains("status 255") && err.ends_with("changed"));
    }

    #[test]
    fn disconnect_and_timeout_end_the_local_process_without_waiting_for_eof() {
        let cancel = AtomicBool::new(true);
        let timeout = Duration::from_secs(3);
        assert!(
            execute(fixture("exit 0"), &cancel, timeout, 16)
                .unwrap_err()
                .contains("disconnected")
        );
        cancel.store(false, Ordering::Relaxed);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                cancel.store(true, Ordering::Relaxed);
            });
            let err = execute(fixture("exec /bin/sleep 30"), &cancel, timeout, 16).unwrap_err();
            assert!(err.contains("disconnected"));
        });
        cancel.store(false, Ordering::Relaxed);
        let start = Instant::now();
        let mut orphan = fixture("/bin/sleep 30 &");
        let (mut witness, lease) = std::os::unix::net::UnixStream::pair().unwrap();
        // Before the run: once the group is killed the other end is closed, and
        // macOS refuses SO_RCVTIMEO on such a socket with EINVAL.
        witness.set_read_timeout(Some(timeout)).unwrap();
        // The fixture's descendants keep the lease until they exit. EOF proves
        // disconnect killed the group rather than just returning to the UI.
        unsafe {
            orphan.pre_exec(move || {
                if libc::fcntl(lease.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let err = execute(orphan, &cancel, Duration::from_millis(50), 16).unwrap_err();
        assert!(err.contains("timed out"));
        assert_eq!(witness.read(&mut [0]).unwrap(), 0);
        assert!(start.elapsed() < timeout);
    }
}
