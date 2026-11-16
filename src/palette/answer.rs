//! Quick answers in the palette: a question goes to a small fast model with
//! what Arakun already knows about it (matching memory, the failed run on the
//! branch), and the answer streams in under the query. It goes through Claude
//! Code the way the user set it up: their claude.ai plan, an API key or another
//! endpoint such as a proxy (`agent::claude_auth`). It runs on a thread of its own;
//! the palette polls a channel, so typing never waits.

use std::process::Child;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Small and fast: answers in a second or two for a fraction of a cent.
pub const MODEL: &str = "claude-haiku-5-5";
pub const MODEL_LABEL: &str = "Haiku 5.5";
/// Answers stay short: a paragraph, not an essay.
const MAX_TOKENS: u32 = 450;

const SYSTEM: &str = "You answer questions inside Arakun, a desktop app where people run coding agents. \
Answer in the language of the question, in at most four short sentences, plain text. \
Put code, file paths, commands and identifiers in backticks. Use only the context given and say \
plainly when it is not enough to be sure. Never invent file names or run numbers.";

/// What the question is about, gathered by the palette.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub question: String,
    pub project: String,
    pub branch: String,
    /// (id, title, body) of matching memories.
    pub memories: Vec<(String, String, String)>,
    /// (branch, "relay CI #4812 failed 1h ago", log tail).
    pub failures: Vec<(String, String, Vec<String>)>,
    /// (path, line, text) of a file the question names.
    pub file: Option<(String, u32, String)>,
}

/// One answer as it streams.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub question: String,
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
    /// (icon, label) of what it read: `CI #4812`, `memory #325`.
    pub sources: Vec<(&'static str, String)>,
    /// The failed run it read, for "Fix it in a new session".
    pub failure: Option<String>,
    pub tokens: (u64, u64),
}

pub enum Event {
    Text(String),
    Done { input: u64, output: u64 },
    Failed(String),
}

impl Answer {
    /// Takes one event; true when the answer is over.
    pub fn take(&mut self, ev: Event) -> bool {
        match ev {
            Event::Text(t) => {
                self.text.push_str(&t);
                false
            }
            Event::Done { input, output } => {
                self.done = true;
                self.tokens = (input, output);
                true
            }
            Event::Failed(why) => {
                self.done = true;
                self.error = Some(why);
                true
            }
        }
    }
}

/// The user message: the question, then the context in tagged blocks.
pub fn prompt(ctx: &Context) -> String {
    let mut out = format!(
        "Question: {}\n\nProject: {} (branch {})\n",
        ctx.question, ctx.project, ctx.branch
    );
    for (branch, what, log) in &ctx.failures {
        out.push_str(&format!(
            "\n<failed_run branch=\"{branch}\">\n{what}\n{}\n</failed_run>\n",
            log.iter()
                .rev()
                .take(40)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    for (id, title, body) in &ctx.memories {
        let body: String = body.chars().take(1200).collect();
        out.push_str(&format!(
            "\n<memory id=\"{id}\" title=\"{title}\">\n{body}\n</memory>\n"
        ));
    }
    if let Some((path, line, text)) = &ctx.file {
        out.push_str(&format!(
            "\n<file path=\"{path}\" line=\"{line}\">\n{text}\n</file>\n"
        ));
    }
    out
}

/// The Messages request.
pub fn body(ctx: &Context) -> Value {
    json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "output_config": {"effort": "low"},
        // A quick answer needs no reasoning pass: it would spend the budget before the text.
        "thinking": {"type": "disabled"},
        "system": SYSTEM,
        "messages": [{"role": "user", "content": prompt(ctx)}],
    })
}

/// Text and the end of one server-sent event's `data:` payload.
pub fn parse_event(data: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(data).ok()?;
    match v["type"].as_str()? {
        "content_block_delta" if v["delta"]["type"] == "text_delta" => {
            Some(Event::Text(v["delta"]["text"].as_str()?.to_string()))
        }
        "message_delta" => Some(Event::Done {
            input: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        }),
        "error" => Some(Event::Failed(
            v["error"]["message"]
                .as_str()
                .unwrap_or("the API failed")
                .to_string(),
        )),
        _ => None,
    }
}

/// How long an answer may take before its process is stopped.
const ANSWER_LIMIT: Duration = Duration::from_secs(60);
/// Reading a sentence as a rule is shorter work.
const RULE_LIMIT: Duration = Duration::from_secs(30);

/// The Claude Code process behind one request. Dropping it stops the
/// process: each one holds a few hundred MB, and a request the person
/// typed past is not worth finishing.
pub struct Job(Arc<Shared>);

/// The thread's side of a `Job`: where it records its process.
struct Watch(Arc<Shared>);

#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    /// Wakes the time-limit thread when the process is done.
    over: Condvar,
}

#[derive(Default)]
struct Slot {
    /// Set only while the process is unreaped, so a stop never hits a reused pid.
    pid: Option<u32>,
    dropped: bool,
    timed_out: bool,
}

fn job() -> (Job, Watch) {
    let shared = Arc::new(Shared::default());
    (Job(shared.clone()), Watch(shared))
}

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Slot> {
    shared.slot.lock().unwrap_or_else(|e| e.into_inner())
}

impl Watch {
    /// Records the process and gives it `limit` to finish. None if the job
    /// was already dropped; the process is then stopped and reaped at once.
    fn started(&self, child: Child, limit: Duration) -> Option<Proc<'_>> {
        let proc = Proc { child, watch: self };
        let mut slot = lock(&self.0);
        if slot.dropped {
            drop(slot);
            return None;
        }
        slot.pid = Some(proc.child.id());
        drop(slot);
        let shared = self.0.clone();
        let _ = std::thread::Builder::new()
            .name("palette-limit".into())
            .spawn(move || {
                let deadline = Instant::now() + limit;
                let mut slot = lock(&shared);
                while let Some(pid) = slot.pid {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        slot.timed_out = true;
                        crate::platform::terminate(pid);
                        return;
                    }
                    slot = shared
                        .over
                        .wait_timeout(slot, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            });
        Some(proc)
    }

    fn timed_out(&self) -> bool {
        lock(&self.0).timed_out
    }
}

/// A started process that is always reaped: on drop it is forgotten by its
/// `Job` first, then stopped if still running, then waited for, on every path.
struct Proc<'a> {
    child: Child,
    watch: &'a Watch,
}

impl Drop for Proc<'_> {
    fn drop(&mut self) {
        // Under the lock: after this neither the job nor the time limit
        // signals the pid, which stays ours until the wait below.
        lock(&self.watch.0).pid = None;
        self.watch.0.over.notify_all();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let mut slot = lock(&self.0);
        slot.dropped = true;
        if let Some(pid) = slot.pid {
            crate::platform::terminate(pid);
        }
    }
}

/// Starts answering on a thread; the answer fills from the channel, and
/// dropping the `Job` stops it.
pub fn start(ctx: Context) -> (Answer, mpsc::Receiver<Event>, Job) {
    let mut sources: Vec<(&'static str, String)> = Vec::new();
    let mut failure = None;
    for (_, what, _) in &ctx.failures {
        let short = what.split(" failed").next().unwrap_or(what).to_string();
        sources.push(("activity01", short));
        failure.get_or_insert_with(|| what.clone());
    }
    for (id, _, _) in &ctx.memories {
        sources.push(("bot", format!("memory {id}")));
    }
    if let Some((path, line, _)) = &ctx.file {
        sources.push(("file01", format!("{path}:{line}")));
    }
    let answer = Answer {
        question: ctx.question.clone(),
        sources,
        failure,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel();
    let (job, mine) = job();
    let _ = std::thread::Builder::new()
        .name("palette-answer".into())
        .spawn(move || run(&ctx, &tx, &mine));
    (answer, rx, job)
}

const RULE_SYSTEM: &str = "You turn one sentence into an automation rule for Arakun, a desktop app for coding agents. \
Triggers: pr_checks_pass (a pull request's checks pass; needs its number), pr_merged (needs its number), \
ci_failed (CI fails on a branch; needs the branch), session_finished (an agent session finishes; give its title as session), \
at (a reminder or timer: minutes is how many minutes from now, counted from the local time given; its step is notify with what to remember). \
Steps, in order: merge (only after pr_checks_pass; text is squash, merge or rebase), notify (text is the notice), \
run (text is a shell command), agent (text is the prompt for a new agent session). \
hours is how long the rule waits, 24 unless the sentence says. If the sentence is not a rule, set understood to false. \
Unused trigger fields are 0 or empty strings.";

/// Asks the small model to read a sentence as a rule; the answer is JSON
/// in `rules::schema()`, still to be checked with `rules::from_json`.
pub fn start_rule(
    sentence: String,
    sessions: Vec<String>,
) -> (mpsc::Receiver<Result<Value, String>>, Job) {
    let (tx, rx) = mpsc::channel();
    let (job, mine) = job();
    let _ = std::thread::Builder::new()
        .name("palette-rule".into())
        .spawn(move || {
            let _ = tx.send(read_rule(&sentence, &sessions, &mine));
        });
    (rx, job)
}

pub fn rule_body(sentence: &str, sessions: &[String]) -> Value {
    let known = if sessions.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nSessions: {}",
            sessions
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    json!({
        "model": MODEL,
        "max_tokens": 300,
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": super::rules::schema()}
        },
        "thinking": {"type": "disabled"},
        "system": RULE_SYSTEM,
        "messages": [{"role": "user", "content": format!("{sentence}{known}\n\nLocal time now: {}", local_now())}],
    })
}

/// `Saturday 14:05` in local time, so "at 5pm" becomes minutes from now.
fn local_now() -> String {
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    match crate::platform::local_time(crate::memory::now()) {
        Some(l) => format!(
            "{} {:02}:{:02}",
            DAYS[l.wday.rem_euclid(7) as usize],
            l.hour,
            l.minute
        ),
        None => String::new(),
    }
}

fn read_rule(sentence: &str, sessions: &[String], job: &Watch) -> Result<Value, String> {
    let request = rule_body(sentence, sessions);
    let schema = &request["output_config"]["format"]["schema"];
    let child = claude_code(&request, Some(schema))?;
    let Some(mut proc) = job.started(child, RULE_LIMIT) else {
        return Err("stopped".into());
    };
    let stdout = proc.child.stdout.take().ok_or("no output")?;
    use std::io::{BufRead as _, BufReader};
    let mut last = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&line)
            && v["type"] == "result"
        {
            last = Some(v);
        }
    }
    drop(proc);
    if job.timed_out() {
        return Err(too_slow(RULE_LIMIT));
    }
    let v = last.ok_or("Claude Code gave no answer")?;
    if v["is_error"].as_bool().unwrap_or(false) {
        return Err(v["result"]
            .as_str()
            .unwrap_or("Claude Code failed")
            .to_string());
    }
    if v["structured_output"].is_object() {
        return Ok(v["structured_output"].clone());
    }
    serde_json::from_str(v["result"].as_str().unwrap_or_default())
        .map_err(|e| format!("not JSON: {e}"))
}

/// What a request that passed its time limit says.
fn too_slow(limit: Duration) -> String {
    format!(
        "Claude Code took longer than {} s, so it was stopped.",
        limit.as_secs()
    )
}

/// Starts Claude Code for one request built as a Messages body (its system
/// prompt, its user message and its model), with no tools, settings, MCP
/// servers or saved session: just the model, on the user's Claude account or key.
fn claude_code(request: &Value, schema: Option<&Value>) -> Result<std::process::Child, String> {
    let bin = crate::agent::claude_code_binary().ok_or_else(no_key)?;
    let mut cmd = crate::platform::command(&bin);
    cmd.args([
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ])
    .args(["--model", request["model"].as_str().unwrap_or(MODEL)])
    .args([
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "",
    ])
    .args(["--no-session-persistence", "--disable-slash-commands"])
    .args([
        "--system-prompt",
        request["system"].as_str().unwrap_or(SYSTEM),
    ]);
    if let Some(schema) = schema {
        cmd.args(["--json-schema", &schema.to_string()]);
    }
    if let Some(max) = request["max_tokens"].as_u64() {
        cmd.env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", max.to_string());
    }
    crate::agent::claude_auth::one_shot(&mut cmd);
    // Away from any project, so no CLAUDE.md is read into a quick answer.
    cmd.current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start Claude Code ({e})"))?;
    let prompt = request["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    feed(&mut child, prompt)?;
    Ok(child)
}

/// Writes `input` to the child's stdin on its own thread and closes it after,
/// which is the child's end of input. Written here, a prompt larger than the
/// pipe holds would block until the child reads it, before the caller has
/// recorded the child for its time limit and for a stop. A write that fails
/// ends the thread when the child exits or is stopped.
fn feed(child: &mut Child, input: String) -> Result<(), String> {
    use std::io::Write as _;
    let Some(mut stdin) = child.stdin.take() else {
        return Ok(());
    };
    let fed = std::thread::Builder::new()
        .name("palette-prompt".into())
        .spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    if let Err(e) = fed {
        // Without its prompt it would wait on stdin with no limit set yet.
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("could not start Claude Code ({e})"));
    }
    Ok(())
}

/// One line of Claude Code's `stream-json` output as an answer event.
pub fn parse_line(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v["type"].as_str()? {
        // Its own end-of-message usage is left for the final result line.
        "stream_event" => {
            parse_event(&v["event"].to_string()).filter(|e| !matches!(e, Event::Done { .. }))
        }
        "result" if v["is_error"].as_bool().unwrap_or(false) => Some(Event::Failed(
            v["result"]
                .as_str()
                .unwrap_or("Claude Code failed")
                .to_string(),
        )),
        "result" => Some(Event::Done {
            input: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        }),
        _ => None,
    }
}

/// What to do when Claude Code isn't there to answer.
pub fn no_key() -> String {
    "Quick answers go through Claude Code: install it, then sign in with `claude /login` or set up an API key.".to_string()
}

fn run(ctx: &Context, tx: &mpsc::Sender<Event>, job: &Watch) {
    let child = match claude_code(&body(ctx), None) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Event::Failed(e));
            return;
        }
    };
    let Some(mut proc) = job.started(child, ANSWER_LIMIT) else {
        return;
    };
    let Some(stdout) = proc.child.stdout.take() else {
        let _ = tx.send(Event::Failed("no output".into()));
        return;
    };
    use std::io::{BufRead as _, BufReader};
    let mut finished = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Some(ev) = parse_line(&line) else {
            continue;
        };
        finished |= matches!(ev, Event::Done { .. } | Event::Failed(_));
        if tx.send(ev).is_err() {
            // The palette closed or the question changed; `proc` stops and reaps it.
            return;
        }
    }
    // Its output closed: it has ended or is about to. The status is read
    // before `proc` goes, which would kill it if it hadn't.
    let code = if finished {
        None
    } else {
        proc.child.wait().ok().map(|s| s.code())
    };
    drop(proc);
    if job.timed_out() {
        let _ = tx.send(Event::Failed(too_slow(ANSWER_LIMIT)));
    } else if !finished {
        // Without its result line the answer is cut short, not done.
        let _ = tx.send(Event::Failed(cut_short(code.flatten())));
    }
}

/// Why an answer stopped without Claude Code's result line.
fn cut_short(code: Option<i32>) -> String {
    match code {
        Some(0) => "Claude Code stopped without finishing the answer.".into(),
        Some(c) => format!("Claude Code stopped with exit code {c} before finishing the answer."),
        None => "Claude Code was stopped before finishing the answer.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_small_and_streams() {
        let ctx = Context {
            question: "¿por qué falló el CI de phone-local?".into(),
            project: "app".into(),
            branch: "phone-local".into(),
            memories: vec![(
                "#325".into(),
                "Relay deploy".into(),
                "wrangler deploy".into(),
            )],
            failures: vec![(
                "phone-local".into(),
                "relay CI #4812 failed 1h ago".into(),
                vec!["src/index.ts(88,14): error TS2345".into()],
            )],
            file: None,
        };
        let b = body(&ctx);
        assert_eq!(b["model"], MODEL);
        assert_eq!(b["stream"], true);
        assert!(b["max_tokens"].as_u64().unwrap() <= 500);
        let p = b["messages"][0]["content"].as_str().unwrap();
        assert!(p.contains("error TS2345"));
        assert!(p.contains("<memory id=\"#325\""));
        let (a, _rx) = {
            // Sources are known before any byte arrives; no thread needed to check them.
            let mut a = Answer::default();
            a.sources.push(("activity01", "relay CI #4812".into()));
            (a, ())
        };
        assert_eq!(a.sources[0].1, "relay CI #4812");
    }

    #[test]
    fn the_rule_request_asks_for_the_schema() {
        let b = rule_body("mergea #64 cuando pasen", &["Sidebar redesign".into()]);
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            b["output_config"]["format"]["schema"]["additionalProperties"],
            false
        );
        assert!(
            b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Sidebar redesign")
        );
    }

    #[test]
    fn stream_events_are_read() {
        let t = parse_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Falló"}}"#,
        );
        assert!(matches!(t, Some(Event::Text(ref s)) if s == "Falló"));
        let d = parse_event(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":9,"output_tokens":40}}"#,
        );
        assert!(matches!(
            d,
            Some(Event::Done {
                input: 9,
                output: 40
            })
        ));
        let e = parse_event(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert!(matches!(e, Some(Event::Failed(ref s)) if s == "Overloaded"));
        assert!(parse_event(r#"{"type":"ping"}"#).is_none());
        assert!(parse_event("not json").is_none());
    }

    #[test]
    fn claude_code_lines_are_read() {
        let t = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"4"}}}"#,
        );
        assert!(matches!(t, Some(Event::Text(ref s)) if s == "4"));
        let d = parse_line(
            r#"{"type":"result","is_error":false,"result":"4","usage":{"input_tokens":2,"output_tokens":5}}"#,
        );
        assert!(matches!(
            d,
            Some(Event::Done {
                input: 2,
                output: 5
            })
        ));
        let e = parse_line(r#"{"type":"result","is_error":true,"result":"Not logged in"}"#);
        assert!(matches!(e, Some(Event::Failed(ref s)) if s == "Not logged in"));
        assert!(parse_line(r#"{"type":"system","subtype":"init"}"#).is_none());
    }

    #[test]
    fn an_answer_cut_short_says_how() {
        assert!(cut_short(Some(0)).contains("without finishing"));
        assert!(cut_short(Some(2)).contains("exit code 2"));
        assert!(cut_short(None).contains("was stopped"));
    }

    #[test]
    fn an_answer_fills_and_ends() {
        let mut a = Answer::default();
        assert!(!a.take(Event::Text("a".into())));
        assert!(!a.take(Event::Text("b".into())));
        assert!(a.take(Event::Done {
            input: 1,
            output: 2
        }));
        assert_eq!(a.text, "ab");
        assert!(a.done);
        let mut b = Answer::default();
        assert!(b.take(Event::Failed("x".into())));
        assert_eq!(b.error.as_deref(), Some("x"));
    }

    #[cfg(unix)]
    fn sleep(secs: &str) -> Child {
        std::process::Command::new("sleep")
            .arg(secs)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Whether `pid` is still a process of ours, zombie or not.
    #[cfg(unix)]
    fn exists(pid: u32) -> bool {
        // SAFETY: signal 0 only checks the pid.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_job_stops_and_reaps_its_process() {
        let (job, watch) = job();
        let proc = watch.started(sleep("30"), ANSWER_LIMIT).unwrap();
        let pid = proc.child.id();
        let t = Instant::now();
        drop(job);
        // The reader sees the end of output and drops its process: reaped.
        drop(proc);
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(!exists(pid), "no zombie left");
        // A process started after the job was dropped is stopped right away.
        let late = sleep("30");
        let pid = late.id();
        assert!(watch.started(late, ANSWER_LIMIT).is_none());
        assert!(!exists(pid));
    }

    #[cfg(unix)]
    #[test]
    fn a_slow_request_is_stopped_at_its_limit() {
        let (_job, watch) = job();
        let mut proc = watch
            .started(sleep("30"), Duration::from_millis(200))
            .unwrap();
        let pid = proc.child.id();
        let t = Instant::now();
        // As the readers do: output ends when the limit stops the process.
        use std::io::Read as _;
        let _ = proc
            .child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut Vec::new());
        drop(proc);
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(watch.timed_out());
        assert!(!exists(pid));
        assert!(too_slow(RULE_LIMIT).contains("30 s"));
    }

    #[cfg(unix)]
    #[test]
    fn a_large_prompt_does_not_hold_up_the_start() {
        // `sleep` never reads stdin: a write of more than a pipe holds would
        // block here, before the process could be given its limit.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let t = Instant::now();
        feed(&mut child, "x".repeat(4 << 20)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(child.stdin.is_none());
        let (_job, watch) = job();
        let proc = watch.started(child, ANSWER_LIMIT).unwrap();
        let pid = proc.child.id();
        // Stopping it ends the writer too, with a broken pipe.
        drop(proc);
        assert!(!exists(pid));
    }
}
