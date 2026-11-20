//! `arakun shrink [--] <command…>`: runs a command and prints a short summary of
//! its output, for agents whose own tools would put all of it in their context.
//! Test runs lose their passing tests and build progress, logs their repeated
//! lines, JSON becomes its outline and CSV its header and a few rows. The whole
//! output goes to a file, and the summary ends with its path, so nothing is lost.
//! Works without the app.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

const USAGE: &str = "\
usage: arakun shrink [--] <command…>

Runs the command line in the shell and prints a summary of what it printed:
failures and the result line for a test run, repeated lines collapsed for a log,
the outline of JSON, the header and a few rows of CSV. The whole output goes to a
file whose path ends the summary. Exits with the command's exit code.";

/// Output up to this long is printed whole.
const SMALL: usize = 4_000;
/// The longest summary; past it a short head and the tail are kept.
const MAX: usize = 12_000;
/// Output held in memory to summarize; the file gets all of it.
const HELD: usize = 16 << 20;
/// Runs of at least this many alike lines collapse.
const RUN: usize = 3;

pub fn cli(args: &[String]) -> i32 {
    let args = match args.first().map(String::as_str) {
        None => {
            eprintln!("{USAGE}");
            return 2;
        }
        Some(a) if crate::control::is_help(a) => {
            println!("{USAGE}");
            return 0;
        }
        Some("--") => &args[1..],
        Some(a) if a.starts_with('-') => {
            eprintln!(
                "arakun shrink: unknown flag {a} (a command that starts with a dash needs -- before it)"
            );
            return 2;
        }
        Some(_) => args,
    };
    if args.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    match run(&args.join(" ")) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("arakun shrink: {e}");
            1
        }
    }
}

fn run(line: &str) -> std::io::Result<i32> {
    let (path, file) = saved_file()?;
    // One pipe for both, so errors stay where they were among the other lines.
    let (mut reader, writer) = std::io::pipe()?;
    let mut cmd = crate::platform::shell_command(line);
    cmd.stdout(writer.try_clone()?).stderr(writer);
    let mut child = cmd.spawn()?;
    // The command keeps its copies of the write end; the read only ends without them.
    drop(cmd);
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let got = match capture(&mut child, &rx, file) {
        Ok(got) => got,
        Err(e) => {
            // A command left running would go on changing files after we report failure.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let code = exit_code(got.status);
    let text = String::from_utf8_lossy(&got.held);
    let out = std::io::stdout();
    let mut out = out.lock();
    if got.total <= SMALL && !got.stopped {
        let _ = std::fs::remove_file(&path);
        out.write_all(text.as_bytes())?;
        return Ok(code);
    }
    let summary = if got.total <= SMALL {
        Summary {
            text: text.into_owned(),
            notes: Vec::new(),
        }
    } else {
        summarize(&text)
    };
    let mut what = summary.notes;
    if got.cut {
        what.push(format!("only its last {} MB read", HELD >> 20));
    }
    if got.stopped {
        what.push(
            "reading stopped 1 s after it exited, as something it started kept the output open"
                .into(),
        );
    }
    let what = what.join(", ");
    writeln!(out, "{}", summary.text.trim_end())?;
    writeln!(
        out,
        "[arakun shrink: {} bytes of output, {} shown{}; exit code {code}; {} is in {}]",
        got.total,
        summary.text.len(),
        if what.is_empty() {
            String::new()
        } else {
            format!(" ({what})")
        },
        if got.stopped {
            "the output read"
        } else {
            "the whole output"
        },
        path.display()
    )?;
    Ok(code)
}

/// What [`capture`] read of a command's output.
struct Captured {
    status: std::process::ExitStatus,
    /// The end of the output, at most `2 * HELD` bytes.
    held: Vec<u8>,
    total: usize,
    /// The start of the output fell out of `held`.
    cut: bool,
    /// Reading stopped at the deadline after the exit, so later output is missing.
    stopped: bool,
}

/// Reads the output into `file` and memory until the pipe closes, or until a
/// second after the command exits.
fn capture(
    child: &mut std::process::Child,
    rx: &mpsc::Receiver<Vec<u8>>,
    mut file: std::fs::File,
) -> std::io::Result<Captured> {
    let (mut held, mut total, mut cut, mut stopped) = (Vec::new(), 0usize, false, false);
    let mut take = |bytes: Vec<u8>| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        total += bytes.len();
        held.extend_from_slice(&bytes);
        // The end matters most: test runners print failures last.
        if held.len() > 2 * HELD {
            held.drain(..held.len() - HELD);
            cut = true;
        }
        Ok(())
    };
    let mut exited = None::<(std::process::ExitStatus, Instant)>;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(bytes) => take(bytes)?,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if exited.is_none()
            && let Some(status) = child.try_wait()?
        {
            exited = Some((status, Instant::now()));
        }
        // Something it left running in the background may hold the pipe open.
        if exited.is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(1)) {
            // Take what is already queued, but not for long: a background
            // writer could keep the queue full forever.
            let until = Instant::now() + Duration::from_millis(200);
            stopped = true;
            while Instant::now() < until {
                match rx.try_recv() {
                    Ok(bytes) => take(bytes)?,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        stopped = false;
                        break;
                    }
                }
            }
            break;
        }
    }
    let status = match exited {
        Some((status, _)) => status,
        None => child.wait()?,
    };
    Ok(Captured {
        status,
        held,
        total,
        cut,
        stopped,
    })
}

/// A new file for the whole output, readable only by this user. A name that is
/// taken (or a link someone put there) is skipped, never opened.
fn saved_file() -> std::io::Result<(PathBuf, std::fs::File)> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let pid = std::process::id();
    let mut n = 0u32;
    loop {
        let name = match n {
            0 => format!("arakun-shrink-{secs}-{pid}.log"),
            _ => format!("arakun-shrink-{secs}-{pid}-{n}.log"),
        };
        let path = std::env::temp_dir().join(name);
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        match open.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// The command's exit code, as a shell reports it: 128 plus the signal that ended it.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

/// A summary of some output, and what was done to it.
#[derive(Debug, Default)]
pub struct Summary {
    pub text: String,
    pub notes: Vec<String>,
}

/// `text` summarized by its format; anything not recognized keeps all its lines
/// but loses colors, progress redraws and runs of alike lines.
pub fn summarize(text: &str) -> Summary {
    let clean = clean(text);
    let mut summary = json(&clean)
        .or_else(|| csv(&clean))
        .unwrap_or_else(|| lines(&clean));
    if summary.text.len() > MAX {
        summary.text = clip(&summary.text, MAX);
        summary.notes.push("middle cut".into());
    }
    summary
}

/// Without terminal colors, and each line as it ended up after `\r` redraws.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: until BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out.lines()
        .map(|l| l.rsplit('\r').find(|s| !s.is_empty()).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lines of a test run or a build that say only that something went well.
fn noise(line: &str) -> Option<&'static str> {
    let t = line.trim();
    // cargo test, and cargo's right-aligned progress.
    if t.starts_with("test ")
        && (t.ends_with(" ... ok") || t.ends_with(" ... ignored") || t.contains(" ... ignored, "))
    {
        return Some("passing tests");
    }
    if line.starts_with(' ')
        && [
            "Compiling ",
            "Checking ",
            "Downloaded ",
            "Downloading ",
            "Fresh ",
            "Blocking waiting",
            "Locking ",
            "Adding ",
            "Updating ",
            "Documenting ",
        ]
        .iter()
        .any(|p| t.starts_with(p))
    {
        return Some("build progress");
    }
    // go test -v.
    if ["=== RUN", "=== PAUSE", "=== CONT", "--- PASS:"]
        .iter()
        .any(|p| t.starts_with(p))
    {
        return Some("passing tests");
    }
    // jest, vitest, mocha.
    if ["✓ ", "√ ", "✔ ", "PASS "].iter().any(|p| t.starts_with(p)) {
        return Some("passing tests");
    }
    // pytest: `x.py::test_a PASSED [ 50%]` and `x.py ....  [ 50%]`, not one with F or E.
    if let Some(head) = t.strip_suffix("%]").and_then(|h| h.rsplit_once('[')) {
        let head = head.0.trim_end();
        if head.ends_with(" PASSED")
            || head
                .rsplit_once(' ')
                .is_some_and(|(_, marks)| marks.chars().all(|c| matches!(c, '.' | 's' | 'x')))
        {
            return Some("passing tests");
        }
    }
    None
}

/// What a line looks like with its numbers taken out, to find runs of alike lines.
fn shape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Plain lines: noise dropped and runs of alike lines collapsed.
fn lines(text: &str) -> Summary {
    let mut dropped: Vec<(&str, usize)> = Vec::new();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| match noise(l) {
            Some(kind) => {
                match dropped.iter_mut().find(|(k, _)| *k == kind) {
                    Some((_, n)) => *n += 1,
                    None => dropped.push((kind, 1)),
                }
                false
            }
            None => true,
        })
        .collect();
    let mut out = Vec::new();
    let mut collapsed = 0;
    let mut i = 0;
    while i < kept.len() {
        let line = kept[i];
        let s = shape(line);
        let mut j = i + 1;
        while j < kept.len() && shape(kept[j]) == s {
            j += 1;
        }
        let n = j - i;
        if line.trim().is_empty() {
            out.push(String::new());
        } else if n < RUN {
            out.extend(kept[i..j].iter().map(|l| l.to_string()));
        } else if kept[i..j].iter().all(|l| *l == line) {
            out.push(format!("{line}  [×{n}]"));
            collapsed += n - 1;
        } else {
            out.push(line.to_string());
            out.push(format!("  … {} lines like these …", n - 2));
            out.push(kept[j - 1].to_string());
            collapsed += n - 2;
        }
        i = j;
    }
    let mut notes: Vec<String> = dropped
        .iter()
        .map(|(kind, n)| format!("{} of {kind} dropped", count(*n, "line")))
        .collect();
    if collapsed > 0 {
        notes.push(format!("{} collapsed", count(collapsed, "repeated line")));
    }
    Summary {
        text: out.join("\n"),
        notes,
    }
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// A short head and the tail of `text`, within about `max` bytes.
fn clip(text: &str, max: usize) -> String {
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let head = floor(max / 5);
    let mut tail = text.len() - (max - head);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n… [{} bytes cut] …\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}

// ---- JSON ------------------------------------------------------------------------

/// Keys of one object shown before the rest are counted.
const KEYS: usize = 40;
/// Nesting shown before an object or array is only counted.
const DEPTH: usize = 6;

fn json(text: &str) -> Option<Summary> {
    let t = text.trim();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let value: Value = serde_json::from_str(t).ok()?;
    let mut out = String::new();
    outline(&value, 0, &mut out);
    (out.len() < t.len()).then(|| Summary {
        text: out,
        notes: vec!["JSON outline: long arrays show their first item".into()],
    })
}

fn outline(v: &Value, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth + 1);
    match v {
        Value::String(s) => {
            let short: String = s.chars().take(80).collect();
            let more = if short.len() < s.len() { "…" } else { "" };
            out.push_str(&serde_json::to_string(&short).unwrap_or_default());
            out.push_str(more);
        }
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Object(m) if depth >= DEPTH => out.push_str(&format!("{{… {} keys}}", m.len())),
        Value::Object(m) => {
            out.push_str("{\n");
            for (k, v) in m.iter().take(KEYS) {
                out.push_str(&format!(
                    "{pad}{}: ",
                    serde_json::to_string(k).unwrap_or_default()
                ));
                outline(v, depth + 1, out);
                out.push_str(",\n");
            }
            if m.len() > KEYS {
                out.push_str(&format!("{pad}… {} more keys\n", m.len() - KEYS));
            }
            out.push_str(&"  ".repeat(depth));
            out.push('}');
        }
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Array(a) if a.iter().all(|v| !v.is_object() && !v.is_array()) => {
            let shown: Vec<String> = a
                .iter()
                .take(10)
                .map(|v| {
                    let mut s = String::new();
                    outline(v, depth, &mut s);
                    s
                })
                .collect();
            let more = if a.len() > 10 {
                format!(", … {} items in all", a.len())
            } else {
                String::new()
            };
            out.push_str(&format!("[{}{more}]", shown.join(", ")));
        }
        Value::Array(a) if depth >= DEPTH => out.push_str(&format!("[… {} items]", a.len())),
        Value::Array(a) => {
            out.push_str("[\n");
            out.push_str(&pad);
            outline(&a[0], depth + 1, out);
            out.push('\n');
            if a.len() > 1 {
                out.push_str(&format!("{pad}… {} items in all\n", a.len()));
            }
            out.push_str(&"  ".repeat(depth));
            out.push(']');
        }
        _ => out.push_str(&v.to_string()),
    }
}

// ---- CSV -------------------------------------------------------------------------

/// Rows shown from the top, and from the end.
const HEAD_ROWS: usize = 5;
const TAIL_ROWS: usize = 2;

fn csv(text: &str) -> Option<Summary> {
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let header = *rows.first()?;
    if rows.len() <= HEAD_ROWS + TAIL_ROWS + 1 {
        return None;
    }
    let sep = if header.contains('\t') { '\t' } else { ',' };
    let width = fields(header, sep);
    if width < 2 {
        return None;
    }
    let alike = rows.iter().filter(|r| fields(r, sep) == width).count();
    // A few ragged lines (a note at the end) still make a table.
    if alike * 10 < rows.len() * 9 {
        return None;
    }
    let body = &rows[1..];
    let mut out = vec![header.to_string()];
    out.extend(body[..HEAD_ROWS].iter().map(|r| r.to_string()));
    out.push(format!("… {} rows in all …", body.len()));
    out.extend(body[body.len() - TAIL_ROWS..].iter().map(|r| r.to_string()));
    Some(Summary {
        text: out.join("\n"),
        notes: vec![format!("table of {} rows", body.len())],
    })
}

/// Fields in a CSV line, with quoted separators not counted.
fn fields(line: &str, sep: char) -> usize {
    let mut quoted = false;
    1 + line
        .chars()
        .filter(|&c| {
            if c == '"' {
                quoted = !quoted;
            }
            c == sep && !quoted
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_redraws_go() {
        assert_eq!(clean("\x1b[1;31merror\x1b[0m: x"), "error: x");
        assert_eq!(clean("\x1b]0;title\x07a"), "a");
        assert_eq!(clean(" 10%\r 50%\r100%\nnext"), "100%\nnext");
        assert_eq!(clean("crlf\r\nline"), "crlf\nline");
    }

    #[test]
    fn a_cargo_run_keeps_its_failures() {
        let mut run =
            String::from("   Compiling foo v0.1.0\n    Finished test\nrunning 300 tests\n");
        for i in 0..300 {
            run.push_str(&format!("test mod{i}::works ... ok\n"));
        }
        run.push_str("test bad::one ... FAILED\n\nfailures:\n\n---- bad::one stdout ----\n");
        run.push_str("thread 'bad::one' panicked at src/bad.rs:3:5:\nassertion failed\n\n");
        run.push_str("test result: FAILED. 300 passed; 1 failed; 0 ignored\n");
        let s = summarize(&run);
        assert!(!s.text.contains("works ... ok"), "{}", s.text);
        assert!(!s.text.contains("Compiling"));
        for kept in [
            "test bad::one ... FAILED",
            "panicked at src/bad.rs:3:5",
            "test result: FAILED",
        ] {
            assert!(s.text.contains(kept), "{kept} missing: {}", s.text);
        }
        assert_eq!(
            s.notes,
            [
                "1 line of build progress dropped",
                "300 lines of passing tests dropped"
            ]
        );
    }

    #[test]
    fn go_jest_and_pytest_lose_their_passes() {
        for pass in [
            "=== RUN   TestA",
            "--- PASS: TestA (0.00s)",
            "  ✓ adds (3 ms)",
            " PASS  src/a.test.js",
            "tests/test_a.py::test_x PASSED                [ 50%]",
            "tests/test_a.py ....s.                        [ 75%]",
        ] {
            assert!(noise(pass).is_some(), "{pass}");
        }
        for fail in [
            "--- FAIL: TestB (0.00s)",
            "  ✕ subtracts (2 ms)",
            " FAIL  src/b.test.js",
            "tests/test_b.py::test_y FAILED                [100%]",
            "tests/test_b.py ..F.                          [100%]",
            "test result: ok. 3 passed",
            "Compiling foo",
        ] {
            assert!(noise(fail).is_none(), "{fail}");
        }
    }

    #[test]
    fn runs_of_alike_lines_collapse() {
        let mut log = String::from("start\n");
        for i in 0..50 {
            log.push_str(&format!("12:00:{i:02} GET /health 200\n"));
        }
        for _ in 0..4 {
            log.push_str("retrying\n");
        }
        log.push_str("a\nb\nend");
        let s = lines(&log);
        assert_eq!(
            s.text,
            "start\n12:00:00 GET /health 200\n  … 48 lines like these …\n12:00:49 GET /health 200\nretrying  [×4]\na\nb\nend"
        );
        assert_eq!(s.notes, ["51 repeated lines collapsed"]);
    }

    #[test]
    fn json_becomes_its_outline() {
        let items: Vec<Value> = (0..500)
            .map(|i| serde_json::json!({"id": i, "name": format!("item {i}"), "tags": ["a", "b"]}))
            .collect();
        let doc = serde_json::json!({"total": 500, "items": items, "next": null});
        let s = summarize(&serde_json::to_string_pretty(&doc).unwrap());
        assert!(s.text.contains("\"total\": 500"), "{}", s.text);
        assert!(s.text.contains("\"name\": \"item 0\""));
        assert!(s.text.contains("… 500 items in all"));
        assert!(!s.text.contains("item 1\""));
        assert!(s.text.len() < 300, "{}", s.text);
    }

    #[test]
    fn small_json_is_left_alone() {
        assert!(json("{\"a\": 1}").is_none());
        assert!(json("[not json").is_none());
    }

    #[test]
    fn csv_keeps_the_header_and_a_few_rows() {
        let mut t = String::from("id,name,note\n");
        for i in 0..1000 {
            t.push_str(&format!("{i},n{i},\"a, b\"\n"));
        }
        let s = summarize(&t);
        assert_eq!(
            s.text,
            "id,name,note\n0,n0,\"a, b\"\n1,n1,\"a, b\"\n2,n2,\"a, b\"\n3,n3,\"a, b\"\n4,n4,\"a, b\"\n… 1000 rows in all …\n998,n998,\"a, b\"\n999,n999,\"a, b\""
        );
        assert_eq!(s.notes, ["table of 1000 rows"]);
        // Prose with commas is not a table.
        assert!(
            csv("Hello, world\nNo commas here\nAnd one, two, three\nx\ny\nz\nw\nv\nu").is_none()
        );
    }

    #[test]
    fn a_long_summary_keeps_its_end() {
        let text: String = (0..5000)
            .map(|i| format!("line {i} {}\n", "x".repeat(i % 7)))
            .collect();
        let s = summarize(&text);
        assert!(s.text.len() <= MAX + 100, "{}", s.text.len());
        assert!(
            s.text.ends_with("line 4999 x"),
            "{}",
            &s.text[s.text.len() - 40..]
        );
        assert!(s.notes.iter().any(|n| n == "middle cut"));
    }

    #[test]
    fn flags_and_help() {
        let s = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli(&s(&[])), 2);
        assert_eq!(cli(&s(&["--help"])), 0);
        assert_eq!(cli(&s(&["--brief", "ls"])), 2);
        assert_eq!(cli(&s(&["--"])), 2);
    }

    #[cfg(unix)]
    #[test]
    fn runs_the_command_and_passes_its_exit_code() {
        let s = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli(&s(&["--", "exit", "3"])), 3);
        assert_eq!(cli(&s(&["true"])), 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_saved_file_is_new_and_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let (a, _fa) = saved_file().unwrap();
        let (b, _fb) = saved_file().unwrap();
        assert_ne!(a, b, "a taken name is skipped");
        let mode = std::fs::metadata(&a).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[cfg(unix)]
    #[test]
    fn a_background_process_does_not_hold_the_read() {
        let (_path, file) = saved_file().unwrap();
        let (mut reader, writer) = std::io::pipe().unwrap();
        let mut cmd = crate::platform::shell_command("sleep 5 & echo done");
        cmd.stdout(writer.try_clone().unwrap()).stderr(writer);
        let mut child = cmd.spawn().unwrap();
        drop(cmd);
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let start = Instant::now();
        let got = capture(&mut child, &rx, file).unwrap();
        let _ = std::fs::remove_file(_path);
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(got.stopped);
        assert_eq!(got.held, b"done\n");
        assert_eq!(exit_code(got.status), 0);
    }
}
