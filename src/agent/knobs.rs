//! Knobs Arakun Lab turns on the Workers AI loop without a rebuild: JSON in
//! `ARAKUN_KNOBS`. Every default is the loop's own behavior, so nothing changes
//! unless a knob is set; unknown keys are ignored with a warning.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct Prune {
    /// Tokens (chars/3) of the newest tool results that are never pruned.
    pub protect_tokens: u64,
    /// Prune only when at least this many tokens would go at once.
    pub min_free: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Knobs {
    /// What the model is sent of a tool's output at most.
    pub tool_chars: usize,
    /// read_file's default `limit`.
    pub read_lines: usize,
    /// Overrides the session's reasoning effort when the model takes it.
    pub reasoning_effort: Option<String>,
    pub max_tokens: u64,
    /// Read-only calls of one reply run at the same time.
    pub parallel_tools: bool,
    /// Names from [`RULES`], appended to the system prompt.
    pub rules: Vec<String>,
    pub read_budget: bool,
    pub bash_tail: bool,
    pub prune: Option<Prune>,
    /// High effort where the next step is a decision, low where it is routine.
    pub adaptive_effort: bool,
    /// One reminder to re-check and stress-test before a prompt ends after edits.
    pub self_test: bool,
    /// Remind the agent to wrap up once the test suite passes after edits.
    pub stop_on_green: bool,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            tool_chars: 20_000,
            read_lines: 2000,
            reasoning_effort: None,
            max_tokens: 65_536,
            parallel_tools: false,
            rules: Vec::new(),
            read_budget: false,
            bash_tail: false,
            prune: None,
            adaptive_effort: false,
            self_test: false,
            stop_on_green: false,
        }
    }
}

const ARGON: &str = "\
# Work method: evidence first, measured decisions
Nobody is available to answer questions: when a real decision is open, pick the most reasonable option, state it in one line, and go on.
1. Inventory before opinions: list the code the request touches, its tests, scripts, logs and earlier attempts. Go to the source, not the summary, and cite file:line for every claim you rely on.
2. Look for existing-but-unmeasured work first (helpers, flags, guards, a fix elsewhere in the repo never wired in). Reuse the project's own tools and test runner.
3. Before running a test, benchmark or check, write down what result means done (command, number, threshold). Do not move it after seeing the result.
4. Treat noise symmetrically: repeat flaky measurements, report median and peak, call a difference inside the noise a tie.
5. Compare against the best known result (current code, best previous attempt), and say plainly when the new version does not beat it.
6. Honest limits: if the goal looks unreachable, say what is reachable and how; never call a line exhausted without a measured ceiling.
7. After every test run, build or benchmark, write one line: the outcome and the decision it leads to, failures included.
8. Keep a short notes file outside the tracked tree ($TMPDIR/notes.md): goal, win rule, what was tried, each measurement and decision. Re-read it after any context loss.
9. After about 30 reads or searches without editing or running anything, write three lines (I know / I decide / I measure) and act. Do not re-read a range you already read unless the file changed.
10. Close with evidence: the verified outcome, every failing or pending check, what remains. Leave the changes in the working tree.";

/// The prompt rules a sweep can switch on, by name.
pub const RULES: [(&str, &str); 7] = [
    (
        "small_task",
        "- If the task is understood after at most 3 reads (~10k tokens), fix it inline: read only what you change and its helpers, \
         see the failing test fail before fixing, then run the focused test and the whole suite in one command.",
    ),
    (
        "no_web_unless_needed",
        "- Do not use web_search or web_fetch unless the task needs external information that is not in the repository.",
    ),
    (
        "parallelize",
        "- Prefer several independent tool calls in one reply (reads, searches) over one call per reply.",
    ),
    (
        "one_larger_read",
        "- Prefer one larger read of a file over many small slices of it.",
    ),
    (
        "checklist",
        "- At the start, list each requirement, example and constraint in the request as a short checklist in your notes.\n\
         - Before finishing, verify each item against your diff and tests, and fix the ones that are missing.",
    ),
    (
        "terse",
        "- Reply terse: answer first, one idea per sentence, no filler or recap. Keep not, never, only and numbers. \
         Quote code and paths exactly. Use full sentences for warnings and irreversible actions.",
    ),
    ("argon", ARGON),
];

const KEYS: [&str; 12] = [
    "adaptive_effort",
    "self_test",
    "stop_on_green",
    "tool_chars",
    "read_lines",
    "reasoning_effort",
    "max_tokens",
    "parallel_tools",
    "rules",
    "read_budget",
    "bash_tail",
    "prune_tool_results",
];

/// The knobs in `raw`, and a warning for each thing ignored.
pub fn parse(raw: &str) -> (Knobs, Vec<String>) {
    let mut k = Knobs::default();
    let mut warn = Vec::new();
    let v: Value = match serde_json::from_str(raw) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            return (
                k,
                vec!["ARAKUN_KNOBS is not a JSON object; using the defaults".into()],
            );
        }
        Err(e) => {
            return (
                k,
                vec![format!(
                    "ARAKUN_KNOBS is not valid JSON ({e}); using the defaults"
                )],
            );
        }
    };
    let o = v.as_object().unwrap();
    for key in o.keys().filter(|key| !KEYS.contains(&key.as_str())) {
        warn.push(format!("unknown knob {key} ignored"));
    }
    let mut int = |key: &str, min: u64| -> Option<u64> {
        let x = o.get(key)?;
        match x.as_u64().filter(|n| *n >= min) {
            Some(n) => Some(n),
            None => {
                warn.push(format!(
                    "{key} must be an integer ≥ {min}, not {x}; ignored"
                ));
                None
            }
        }
    };
    if let Some(n) = int("tool_chars", 100) {
        k.tool_chars = n as usize;
    }
    if let Some(n) = int("read_lines", 1) {
        k.read_lines = n as usize;
    }
    if let Some(n) = int("max_tokens", 256) {
        k.max_tokens = n;
    }
    let prune = o
        .get("prune_tool_results")
        .filter(|p| !p.is_null())
        .map(|p| {
            (
                p.is_object(),
                p["protect_tokens"].as_u64(),
                p["min_free"].as_u64(),
            )
        });
    match prune {
        None => {}
        Some((true, Some(protect_tokens), Some(min_free))) if min_free > 0 => k.prune = Some(Prune { protect_tokens, min_free }),
        Some(_) => warn.push("prune_tool_results must be null or {protect_tokens, min_free} (integers, min_free > 0); ignored".into()),
    }
    let mut flag = |key: &str| match o.get(key) {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(x) => {
            warn.push(format!("{key} must be true or false, not {x}; ignored"));
            false
        }
    };
    k.parallel_tools = flag("parallel_tools");
    k.read_budget = flag("read_budget");
    k.bash_tail = flag("bash_tail");
    k.adaptive_effort = flag("adaptive_effort");
    k.self_test = flag("self_test");
    k.stop_on_green = flag("stop_on_green");
    match o.get("reasoning_effort") {
        None | Some(Value::Null) => {}
        Some(Value::String(e)) if ["none", "low", "high", "max"].contains(&e.as_str()) => {
            k.reasoning_effort = Some(e.clone())
        }
        Some(x) => warn.push(format!(
            "reasoning_effort must be none, low, high or max, not {x}; ignored"
        )),
    }
    match o.get("rules") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for r in list {
                match r.as_str().filter(|r| RULES.iter().any(|(n, _)| n == r)) {
                    Some(r) if !k.rules.iter().any(|x| x == r) => k.rules.push(r.to_string()),
                    Some(_) => {}
                    None => warn.push(format!("unknown rule {r} ignored")),
                }
            }
        }
        Some(x) => warn.push(format!("rules must be an array of names, not {x}; ignored")),
    }
    (k, warn)
}

impl Knobs {
    /// The rules' text, to append to the system prompt; empty without rules.
    pub fn rules_text(&self) -> String {
        let texts: Vec<&str> = self
            .rules
            .iter()
            .filter_map(|r| RULES.iter().find(|(n, _)| n == r))
            .map(|(_, t)| *t)
            .collect();
        texts.join("\n")
    }

    /// The effective knobs as one JSON line, for the log.
    pub fn summary(&self) -> String {
        json!({
            "tool_chars": self.tool_chars,
            "read_lines": self.read_lines,
            "reasoning_effort": self.reasoning_effort,
            "max_tokens": self.max_tokens,
            "parallel_tools": self.parallel_tools,
            "rules": self.rules,
            "read_budget": self.read_budget,
            "bash_tail": self.bash_tail,
            "adaptive_effort": self.adaptive_effort,
            "self_test": self.self_test,
            "stop_on_green": self.stop_on_green,
            "prune_tool_results": self.prune.as_ref().map(|p| json!({"protect_tokens": p.protect_tokens, "min_free": p.min_free})),
        })
        .to_string()
    }
}

/// The process's knobs, from `ARAKUN_KNOBS`; warnings go to stderr once.
pub fn get() -> &'static Knobs {
    static KNOBS: OnceLock<Knobs> = OnceLock::new();
    KNOBS.get_or_init(|| {
        let Some(raw) = std::env::var("ARAKUN_KNOBS")
            .ok()
            .filter(|r| !r.trim().is_empty())
        else {
            return Knobs::default();
        };
        let (k, warnings) = parse(&raw);
        for w in warnings {
            eprintln!("arakun: knobs: {w}");
        }
        k
    })
}

/// Logs the effective knobs once per process, when `ARAKUN_KNOBS` is set.
pub fn log_once() {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    let k = get();
    if std::env::var_os("ARAKUN_KNOBS").is_some() && !LOGGED.swap(true, Ordering::SeqCst) {
        eprintln!("arakun: knobs {}", k.summary());
    }
}

// ---- adaptive effort and self test -------------------------------------------------

pub const SELF_TEST_REMINDER: &str = "Before finishing: re-check every requirement and example from the request against your diff, \
and add a quick test with variants of the given examples that tries to break your change; run the tests.";

/// Whether a tool's result tells the model something went wrong.
pub fn result_failed(tool: &str, content: &str) -> bool {
    if content.starts_with("Error:")
        || content.starts_with("The user declined")
        || content.starts_with("(not run")
    {
        return true;
    }
    if tool != "bash" {
        return false;
    }
    let tail = content.lines().last().unwrap_or("");
    if tail.starts_with('[') && tail.ends_with(']') && tail != "[exit code 0]" {
        return true;
    }
    [
        "test result: FAILED",
        "FAILED",
        "AssertionError",
        "Traceback (most recent call last)",
        "failures:",
        "npm ERR!",
    ]
    .iter()
    .any(|m| content.contains(m))
}

/// What one prompt has done, to choose each call's effort and to time the self test.
#[derive(Debug, Default)]
pub struct Progress {
    calls: u32,
    failed: bool,
    unchecked_edit: bool,
    edited: bool,
    reminded: bool,
}

impl Progress {
    /// "high" for the first call, after a failure, and after an edit nothing has
    /// run against; "low" for the routine rest.
    pub fn effort(&self) -> &'static str {
        if self.calls == 0 || self.failed || self.unchecked_edit {
            "high"
        } else {
            "low"
        }
    }

    /// A model call is about to go out.
    pub fn called(&mut self) {
        self.calls += 1;
    }

    /// The results of one reply's tools are in; `results` are (tool, content).
    pub fn results<'a>(&mut self, results: impl IntoIterator<Item = (&'a str, &'a str)>) {
        self.failed = false;
        for (tool, content) in results {
            let bad = result_failed(tool, content);
            self.failed |= bad;
            match tool {
                "edit_file" | "write_file" if !bad => {
                    self.unchecked_edit = true;
                    self.edited = true;
                }
                "bash" => self.unchecked_edit = false,
                _ => {}
            }
        }
    }

    /// A shell command changed files in the working tree (see `tree_mark`): an edit like any
    /// other, and one nothing has run against yet.
    pub fn tree_changed(&mut self) {
        self.unchecked_edit = true;
        self.edited = true;
    }

    /// The model is about to end the prompt: true the one time the self test is due.
    pub fn self_test_due(&mut self) -> bool {
        if self.edited && !self.reminded {
            self.reminded = true;
            return true;
        }
        false
    }
}

// ---- read budget -------------------------------------------------------------------

/// Lookups past this many without an edit or a command trip the reminder.
const BUDGET_LOOKUPS: u32 = 3;
/// Bytes of read output past this many, likewise.
const BUDGET_BYTES: usize = 40_000;
pub const BUDGET_REMINDER: &str = "<system-reminder>You have explored enough to decide; edit or run something now.</system-reminder>";

/// Exploration since the last edit or command, in one prompt.
#[derive(Debug, Default)]
pub struct ReadBudget {
    lookups: u32,
    bytes: usize,
    fired: bool,
    edited: bool,
    greens_since_edit: u32,
}

pub const GREEN_REMINDER_FIRST: &str = "<system-reminder>All tests just passed (exit code 0). If your test for the request's examples is already in place, write $LAB_NOTES (if set) and finish your turn immediately — the adversarial verifier runs next.</system-reminder>";
pub const GREEN_REMINDER_REPEAT: &str = "<system-reminder>Tests already passed and no files were edited since. Write $LAB_NOTES (if set) and end your turn now.</system-reminder>";

/// What the working tree looks like, to tell that a shell command edited files: the paths
/// `git status` lists (changed and untracked, not ignored), each with its size and its
/// modification time. Models that edit with `sed` or a script never call `edit_file`, so the
/// tool's name says nothing; the tree does. None outside a git repository.
pub fn tree_mark(cwd: &std::path::Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let out = crate::platform::output_within(
        crate::platform::command("git").arg("-C").arg(cwd).args([
            "status",
            "--porcelain=v1",
            "-z",
            "-uall",
            "--no-renames",
        ]),
        None,
        crate::platform::GIT_LOCAL,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for entry in out
        .stdout
        .split(|b| *b == 0)
        .filter(|e| e.len() > 3)
        .take(5000)
    {
        let name = String::from_utf8_lossy(&entry[3..]);
        // what running tests leaves behind in repositories that do not ignore it is not an edit
        if [
            "__pycache__/",
            ".pytest_cache/",
            "node_modules/",
            ".mypy_cache/",
            ".ruff_cache/",
        ]
        .iter()
        .any(|d| name.contains(d))
            || name.ends_with(".pyc")
            || name.ends_with(".coverage")
        {
            continue;
        }
        entry.hash(&mut h);
        let path = cwd.join(name.as_ref());
        if let Ok(m) = std::fs::metadata(&path) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    }
    Some(h.finish())
}

/// Whether a bash command only inspects files (`sed -n`, `cat`, `head`, `tail`, `nl`, `grep`…)
/// without running a compiler, test runner, interpreter, or modifying files.
pub fn bash_is_read_only(cmd: &str) -> bool {
    let t = cmd.trim();
    if t.is_empty() {
        return false;
    }
    let active = [
        "cargo ",
        "rustc",
        "npm ",
        "npx ",
        "bun ",
        "node ",
        "pytest",
        "python",
        "nvcc",
        "ptxas",
        "make",
        "cmake",
        "./",
        "bash ",
        "sh ",
        "dash ",
        "zsh ",
        "git commit",
        "git checkout",
        "git reset",
        "git clean",
        "rm ",
        "mv ",
        "cp ",
        "mkdir ",
        "tee ",
        ">>",
        " > ",
    ];
    if active.iter().any(|k| t.contains(k)) {
        return false;
    }
    [
        "sed ",
        "nl ",
        "cat ",
        "head ",
        "tail ",
        "grep ",
        "rg ",
        "wc ",
        "ls",
        "find ",
        "git status",
        "git diff",
        "git log",
    ]
    .iter()
    .any(|k| t.contains(k))
}

impl ReadBudget {
    /// Counts one tool's result; true the one time the reminder is due.
    pub fn note(&mut self, tool: &str, output_len: usize) -> bool {
        self.note_call(tool, None, output_len)
    }

    /// Counts one tool's result (treating read-only bash slices as lookups); true when the reminder is due.
    pub fn note_call(&mut self, tool: &str, bash_cmd: Option<&str>, output_len: usize) -> bool {
        match tool {
            "bash" if bash_cmd.is_some_and(bash_is_read_only) => {
                self.lookups += 1;
                self.bytes += output_len;
            }
            "edit_file" | "write_file" => {
                self.lookups = 0;
                self.bytes = 0;
                self.edited = true;
                self.greens_since_edit = 0;
            }
            "bash" => {
                self.lookups = 0;
                self.bytes = 0;
            }
            "grep" | "list" | "web_search" => {
                self.lookups += 1;
                self.bytes += output_len;
            }
            "read_file" | "web_fetch" => self.bytes += output_len,
            _ => {}
        }
        if self.fired || (self.lookups <= BUDGET_LOOKUPS && self.bytes <= BUDGET_BYTES) {
            return false;
        }
        self.fired = true;
        true
    }

    /// A shell command changed files in the working tree: what `edit_file` does to the counts.
    pub fn note_edit(&mut self) {
        self.lookups = 0;
        self.bytes = 0;
        self.edited = true;
        self.greens_since_edit = 0;
    }

    /// Tracks green test runs after edits so the agent stops looping on passing tests.
    pub fn note_green_test(
        &mut self,
        tool: &str,
        bash_cmd: Option<&str>,
        content: &str,
    ) -> Option<&'static str> {
        if matches!(tool, "edit_file" | "write_file") && !result_failed(tool, content) {
            self.edited = true;
            self.greens_since_edit = 0;
            return None;
        }
        if !self.edited || tool != "bash" || result_failed(tool, content) {
            return None;
        }
        let cmd = bash_cmd.unwrap_or("");
        if cmd.contains("--no-run") {
            return None;
        }
        let is_test = [
            "cargo test",
            "bun test",
            "node --test",
            "pytest",
            "npm test",
            "npm run test",
        ]
        .iter()
        .any(|k| cmd.contains(k))
            || content.contains("test result: ok.");
        if !is_test {
            return None;
        }
        self.greens_since_edit += 1;
        match self.greens_since_edit {
            0 | 1 => None,
            2 => Some(GREEN_REMINDER_FIRST),
            _ => Some(GREEN_REMINDER_REPEAT),
        }
    }

    /// Uses Cloudflare Clef (`ARAKUN_CLEF_URL`) on the first green test run to decide in ~200 ms
    /// whether the diff already includes both the implementation and new test assertions.
    pub fn note_green_test_with_clef(
        &mut self,
        cwd: &std::path::Path,
        tool: &str,
        bash_cmd: Option<&str>,
        content: &str,
    ) -> Option<String> {
        let base = self.note_green_test(tool, bash_cmd, content);
        if self.greens_since_edit != 1 {
            return base.map(str::to_string);
        }
        let url = std::env::var("ARAKUN_CLEF_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())?;
        let diff = crate::platform::output_within(
            crate::platform::command("git")
                .args(["diff", "-U1"])
                .current_dir(cwd),
            None,
            crate::platform::GIT_LOCAL,
        )
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
        if diff.trim().is_empty() {
            return None;
        }
        let body = json!({
            "model": "clef",
            "state": {
                "git_diff": diff_excerpt(&diff),
                "test_output": &content[..(0..=content.len().min(2000)).rev().find(|&i| content.is_char_boundary(i)).unwrap_or(0)],
            },
            "questions": {
                "impl_ready": {
                    "type": "noul",
                    "instructions": "Does the git_diff contain a substantial implementation addressing multiple cases/functions (rather than a tiny unfinished edit)?",
                    "criteria": {
                        "true": "The diff implements complete logic across the target functions",
                        "false": "The diff is minimal or incomplete"
                    }
                },
                "tests_added": {
                    "type": "noul",
                    "instructions": "Does the git_diff add or update test cases?",
                    "criteria": {
                        "true": "Test assertions were added or updated in the diff",
                        "false": "No test changes in the diff"
                    }
                }
            }
        });
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(4)))
            .http_status_as_error(false)
            .build()
            .into();
        let res = agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes())
            .ok()?;
        let text = res.into_body().read_to_string().ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        let p_impl = v["answers"]["impl_ready"]["noul"].as_f64()?;
        let p_test = v["answers"]["tests_added"]["noul"].as_f64()?;
        eprintln!("arakun: clef green gate: p_impl={p_impl:.2} p_test={p_test:.2}");
        if p_impl >= 0.75 {
            Some(format!(
                "<system-reminder>Clef decision gate (impl={p_impl:.2}, tests={p_test:.2}): implementation is ready and tests pass. Write $LAB_NOTES (if set) and finish your turn immediately — the parallel verifier will run the full adversarial test suite now.</system-reminder>"
            ))
        } else {
            None
        }
    }
}

/// What Clef is sent of a diff: its first 14,000 bytes, or its head and tail
/// when it is longer than about 20,000.
fn diff_excerpt(diff: &str) -> String {
    let cut = (0..=diff.len().min(14_000))
        .rev()
        .find(|&i| diff.is_char_boundary(i))
        .unwrap_or(0);
    let tail_start = (diff.len().saturating_sub(6_000)..=diff.len())
        .find(|&i| diff.is_char_boundary(i))
        .unwrap_or(diff.len());
    if cut < tail_start {
        format!(
            "{}\n…\n{}",
            &diff[..diff.floor_char_boundary(cut.min(8_000))],
            &diff[tail_start..]
        )
    } else {
        diff[..cut].to_string()
    }
}

// ---- bash tail ---------------------------------------------------------------------

/// `text` within about `max` bytes: a short head and the long tail, with where
/// the whole output (`total` bytes) is saved.
pub fn clip_tail(text: &str, max: usize, saved: &str, total: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let head = floor((max / 8).min(2000));
    let mut tail = text.len() - (max - head);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n… [{} bytes cut; the whole output ({} bytes) is in {saved}] …\n{}",
        &text[..head],
        total.max(text.len()) - head - (text.len() - tail),
        total.max(text.len()),
        &text[tail..]
    )
}

// ---- pruning -----------------------------------------------------------------------

pub const PRUNED_PREFIX: &str = "[tool output pruned:";

pub fn pruned_stub(chars: usize) -> String {
    format!("{PRUNED_PREFIX} {chars} chars; rerun the tool if needed]")
}

/// The tool results to stub before the next call: those older than the newest
/// `protect_tokens` (at three chars a token), when together they free at least
/// `min_free` tokens; else none. The last user message and the results of the
/// last reply are never touched.
pub fn prune_plan(messages: &[Value], p: &Prune) -> Vec<usize> {
    let last_assistant = messages
        .iter()
        .rposition(|m| m["role"] == "assistant")
        .unwrap_or(0);
    let tokens = |m: &Value| m["content"].as_str().map_or(0, str::len) as u64 / 3;
    let (mut seen, mut free, mut picked) = (0u64, 0u64, Vec::new());
    for (i, m) in messages.iter().enumerate().rev() {
        if m["role"] != "tool" {
            continue;
        }
        let t = tokens(m);
        if i > last_assistant {
            seen += t;
            continue;
        }
        let pruned = m["content"]
            .as_str()
            .is_some_and(|c| c.starts_with(PRUNED_PREFIX));
        if seen < p.protect_tokens || pruned {
            seen += t;
            continue;
        }
        free += t;
        picked.push(i);
    }
    if free >= p.min_free {
        picked
    } else {
        Vec::new()
    }
}

/// Applies [`prune_plan`]; how many results it stubbed.
pub fn prune(messages: &mut [Value], p: &Prune) -> usize {
    let picked = prune_plan(messages, p);
    for &i in &picked {
        let chars = messages[i]["content"]
            .as_str()
            .map_or(0, |c| c.chars().count());
        messages[i]["content"] = json!(pruned_stub(chars));
    }
    picked.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_knobs_are_the_defaults() {
        let (k, w) = parse("{}");
        assert_eq!(k, Knobs::default());
        assert!(w.is_empty());
        assert_eq!(k.rules_text(), "");
    }

    #[test]
    fn knobs_parse_and_warn() {
        let (k, w) = parse(
            r#"{"tool_chars": 8000, "read_lines": 500, "reasoning_effort": "low", "max_tokens": 32768,
                "parallel_tools": true, "rules": ["small_task", "nope", "small_task", "argon"],
                "read_budget": true, "bash_tail": true, "prune_tool_results": {"protect_tokens": 40000, "min_free": 20000},
                "colour": 1}"#,
        );
        assert_eq!(k.tool_chars, 8000);
        assert_eq!(k.read_lines, 500);
        assert_eq!(k.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(k.max_tokens, 32768);
        assert!(k.parallel_tools && k.read_budget && k.bash_tail);
        assert_eq!(k.rules, vec!["small_task", "argon"]);
        assert_eq!(
            k.prune,
            Some(Prune {
                protect_tokens: 40000,
                min_free: 20000
            })
        );
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|x| x.contains("colour")) && w.iter().any(|x| x.contains("nope")));
        let text = k.rules_text();
        assert!(text.contains("at most 3 reads") && text.contains("Work method"));
        let (k, w) = parse(r#"{"rules": ["terse"]}"#);
        assert!(w.is_empty(), "{w:?}");
        assert!(k.rules_text().starts_with("- Reply terse"));
    }

    #[test]
    fn bad_values_keep_the_defaults() {
        let (k, w) = parse(
            r#"{"tool_chars": "big", "reasoning_effort": "medium", "bash_tail": 1, "prune_tool_results": {"protect_tokens": 1}}"#,
        );
        assert_eq!(k, Knobs::default());
        assert_eq!(w.len(), 4, "{w:?}");
        let (k, w) = parse("not json");
        assert_eq!(k, Knobs::default());
        assert_eq!(w.len(), 1);
        let (k, _) = parse(r#"{"prune_tool_results": null}"#);
        assert_eq!(k.prune, None);
    }

    #[test]
    fn rules_stay_short_but_argon() {
        for (name, text) in RULES.iter().filter(|(n, _)| *n != "argon") {
            assert!(text.len() < 300, "{name} is too long");
        }
    }

    #[test]
    fn adaptive_effort_follows_failures_and_unchecked_edits() {
        let mut p = Progress::default();
        assert_eq!(p.effort(), "high", "the first call");
        p.called();
        p.results([("read_file", "1\tfn main() {}"), ("grep", "a.rs:1:x")]);
        assert_eq!(p.effort(), "low", "routine reads");
        p.called();
        p.results([("edit_file", "Edited a.rs")]);
        assert_eq!(p.effort(), "high", "an edit nothing has run against");
        p.called();
        p.results([("bash", "ok\n[exit code 0]")]);
        assert_eq!(p.effort(), "low", "the command ran after the edit");
        p.called();
        p.results([("bash", "boom\n[exit code 1]")]);
        assert_eq!(p.effort(), "high", "a failed command");
        p.called();
        p.results([("read_file", "Error: no such file")]);
        assert_eq!(p.effort(), "high", "a tool error");
        p.called();
        p.results([("bash", "test result: FAILED. 1 failed\n[exit code 0]")]);
        assert_eq!(p.effort(), "high", "test failure text");
        p.called();
        p.results([("grep", "fine")]);
        assert_eq!(p.effort(), "low", "the failure was dealt with");
        p.called();
        p.results([("bash", "slow\n[timed out]")]);
        assert_eq!(p.effort(), "high", "a stopped command");
    }

    #[test]
    fn self_test_fires_once_and_only_after_edits() {
        let mut p = Progress::default();
        assert!(!p.self_test_due(), "nothing edited");
        p.results([("edit_file", "Error: old text not found")]);
        assert!(!p.self_test_due(), "a failed edit is not an edit");
        p.results([("write_file", "Wrote a.rs")]);
        assert!(p.self_test_due());
        assert!(!p.self_test_due(), "once per prompt");
        assert!(SELF_TEST_REMINDER.starts_with("Before finishing:"));
    }

    #[test]
    fn an_edit_from_the_shell_counts_as_an_edit() {
        // a repository where the "model" edits with a shell command, never with edit_file
        let dir = std::env::temp_dir().join(format!("arakun-tree-mark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            crate::git::unpinned(&mut std::process::Command::new("git"))
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(tree_mark(&dir).is_none(), "not a repository yet");
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "base",
        ]);
        let clean = tree_mark(&dir).expect("a repository");
        assert_eq!(tree_mark(&dir), Some(clean), "reading changes nothing");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let edited = tree_mark(&dir).unwrap();
        assert_ne!(edited, clean, "a tracked file changed");
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        assert_ne!(
            tree_mark(&dir).unwrap(),
            edited,
            "the same file changed again"
        );
        let before = tree_mark(&dir).unwrap();
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        assert_ne!(tree_mark(&dir).unwrap(), before, "a new file");
        let before = tree_mark(&dir).unwrap();
        std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
        std::fs::write(dir.join("__pycache__/a.cpython-311.pyc"), "x").unwrap();
        assert_eq!(
            tree_mark(&dir).unwrap(),
            before,
            "what a test run leaves behind is not an edit"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // and the self test, which waits for an edit, fires after one
        let mut p = Progress::default();
        p.results([("bash", "[exit code 0]")]);
        assert!(
            !p.self_test_due(),
            "a command that changed nothing is not an edit"
        );
        p.tree_changed();
        assert_eq!(p.effort(), "high", "nothing has run against the edit");
        assert!(p.self_test_due());
        let mut b = ReadBudget::default();
        assert!(
            b.note_green_test("bash", Some("pytest -q"), "[exit code 0]")
                .is_none(),
            "no edit yet"
        );
        b.note_edit();
        b.note_green_test("bash", Some("pytest -q"), "[exit code 0]");
        assert!(
            b.note_green_test("bash", Some("pytest -q"), "[exit code 0]")
                .is_some(),
            "second green run after a shell edit"
        );
    }

    #[test]
    fn checklist_rule_is_three_lines_at_most() {
        let (k, w) =
            parse(r#"{"adaptive_effort": true, "self_test": true, "rules": ["checklist"]}"#);
        assert!(w.is_empty(), "{w:?}");
        assert!(k.adaptive_effort && k.self_test);
        assert!(k.rules_text().lines().count() <= 3);
    }

    #[test]
    fn read_budget_fires_once_after_four_lookups() {
        let mut b = ReadBudget::default();
        assert!(!b.note("grep", 100));
        assert!(!b.note("list", 100));
        assert!(!b.note("grep", 100));
        assert!(b.note("grep", 100), "the fourth lookup trips it");
        assert!(!b.note("grep", 100), "only once per prompt");
    }

    #[test]
    fn read_budget_counts_bytes_and_edits_reset_it() {
        let mut b = ReadBudget::default();
        assert!(!b.note("read_file", 30_000));
        assert!(!b.note("edit_file", 50));
        assert!(!b.note("read_file", 30_000), "the edit reset the count");
        assert!(!b.note("bash", 50_000), "commands don't count as reads");
        assert!(!b.note("read_file", 39_000));
        assert!(b.note("read_file", 2_000));
        let mut b2 = ReadBudget::default();
        assert!(!b2.note_call("bash", Some("sed -n '1,240p' src/lib.rs"), 8_000));
        assert!(!b2.note_call("bash", Some("sed -n '240,520p' src/lib.rs"), 8_000));
        assert!(!b2.note_call("bash", Some("grep -n foo src/lib.rs"), 2_000));
        assert!(
            b2.note_call("bash", Some("sed -n '520,820p' src/lib.rs"), 8_000),
            "read-only bash counts toward lookups"
        );
    }

    #[test]
    fn bash_tail_keeps_the_end_and_names_the_file() {
        let text: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let out = clip_tail(&text, 2000, "/tmp/x.log", text.len());
        assert!(out.len() < 2200, "{}", out.len());
        assert!(out.starts_with("line 0\n"));
        assert!(out.ends_with("line 4999\n"));
        assert!(out.contains("/tmp/x.log") && out.contains(&format!("({} bytes)", text.len())));
        assert_eq!(clip_tail("short", 2000, "/tmp/x", 5), "short");
        // run_bash keeps only a head and a tail of long output: the counts are the real ones.
        let long = clip_tail(&text, 2000, "/p", 100_000);
        assert!(
            long.contains("[98000 bytes cut; the whole output (100000 bytes)"),
            "{long}"
        );
        let wide = "ñ".repeat(3000);
        assert!(clip_tail(&wide, 1001, "/p", 1).ends_with('ñ'));
    }

    fn tool(n: usize) -> Value {
        json!({"role": "tool", "tool_call_id": "x", "content": "a".repeat(n)})
    }

    #[test]
    fn prune_takes_old_results_only_past_the_threshold() {
        let call = json!({"role": "assistant", "content": null, "tool_calls": [{"id": "x"}]});
        let mut m = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": "do it"}),
        ];
        for _ in 0..4 {
            m.push(call.clone());
            m.push(tool(30_000)); // 10k tokens each
        }
        m.push(call.clone());
        m.push(tool(90_000)); // the last reply's result: always kept
        // The last result (30k tokens) and the one before it fill the 35k protected.
        let p = Prune {
            protect_tokens: 35_000,
            min_free: 15_000,
        };
        assert_eq!(prune_plan(&m, &p), vec![7, 5, 3]);
        assert!(
            prune_plan(
                &m,
                &Prune {
                    protect_tokens: 35_000,
                    min_free: 40_000
                }
            )
            .is_empty(),
            "too little to free"
        );
        assert_eq!(prune(&mut m, &p), 3);
        assert_eq!(m[3]["content"], pruned_stub(30_000));
        assert_eq!(m[11]["content"].as_str().unwrap().len(), 90_000);
        assert_eq!(m[1]["content"], "do it");
        assert!(prune_plan(&m, &p).is_empty(), "stubs are not pruned again");
    }

    #[test]
    fn diff_excerpt_cuts_the_head_at_a_char() {
        // A long diff of non-ASCII text, with a char across byte 8,000.
        let diff = format!("{}{}", "x".repeat(7_999), "é".repeat(10_000));
        let excerpt = diff_excerpt(&diff);
        let (head, tail) = excerpt.split_once("\n…\n").unwrap();
        assert_eq!(head, "x".repeat(7_999));
        assert!(diff.ends_with(tail) && tail.len() <= 6_000);
    }
}
