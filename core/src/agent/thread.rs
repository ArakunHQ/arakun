//! One agent session with one agent process, as a GPUI entity. Claude speaks
//! ACP; Droid speaks its native `stream-jsonrpc` (see `native.rs`) unless
//! `ARAKUN_DROID_ACP=1` keeps it on ACP too.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt as _;
use futures::channel::mpsc as fmpsc;
use futures::future::{FutureExt as _, Shared};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, SharedString, Task, WeakEntity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AgentKind;
use super::attach;
use super::diff;
use super::launch;
use super::ledger;
use super::loops::{self, Stuck};
use super::rpc::{Connection, Dialect, Incoming, RpcError, unix_now, uuid_v4};
use super::seats;
use super::shell_diff::{self, ShellChange, Snapshot};

// Children of this module so they can reach the private state of `Thread`.
#[path = "acp.rs"]
mod acp;
#[path = "native.rs"]
mod native;
#[path = "subagents.rs"]
mod subagents;
pub use native::{
    AskUserRequest, Backend, ModelInfo, NativeSettings, Question, RewindPlan, Setting,
    SettingOption, SpecPlan, apply_setting, effort_name, fmt_count, setting_label, setting_options,
};
pub use subagents::{Subagent, SubagentRecord, SubagentState};

#[derive(Clone, Debug, PartialEq)]
pub enum ThreadStatus {
    Connecting,
    Idle,
    Working,
    NeedsInput,
    Error(String),
}

/// Where a session failed, for crash reports. Only fixed labels and numbers,
/// never the message: that can hold a project's name or a credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedAt {
    /// Finding or installing the agent's program.
    Resolve,
    /// Starting its process.
    Spawn,
    /// The `initialize` request, with the error's JSON-RPC code.
    Initialize(i64),
    /// Opening the session; the code is `None` when the reply had no session id.
    Session(Option<i64>),
    /// A turn, with the error's JSON-RPC code.
    Turn(i64),
    /// The process stopped answering during a turn.
    Disconnected,
    /// The process exited; its exit code, or the signal that ended it (negative).
    Exited(Option<i64>),
}

impl FailedAt {
    pub fn stage(self) -> &'static str {
        match self {
            FailedAt::Resolve => "resolve",
            FailedAt::Spawn => "spawn",
            FailedAt::Initialize(_) => "initialize",
            FailedAt::Session(_) => "session",
            FailedAt::Turn(_) => "turn",
            FailedAt::Disconnected => "disconnected",
            FailedAt::Exited(_) => "exited",
        }
    }

    /// The JSON-RPC error code, or the exit code for [`FailedAt::Exited`].
    pub fn code(self) -> Option<i64> {
        match self {
            FailedAt::Initialize(c) | FailedAt::Turn(c) => Some(c),
            FailedAt::Session(c) | FailedAt::Exited(c) => c,
            _ => None,
        }
    }
}

/// The code in a process's exit status as Rust prints it: `exit status: 1` on
/// Unix, `exit code: 1` on Windows, and `signal: 9 (SIGKILL)` as -9. Windows
/// prints a crash's NTSTATUS in hex (`exit code: 0xc0000005`).
fn exit_code(status: &str) -> Option<i64> {
    let (head, rest) = status.split_once(':')?;
    let word = rest.split_whitespace().next()?;
    let n = match word.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => word.parse().ok()?,
    };
    match head.trim() {
        "exit status" | "exit code" => Some(n),
        "signal" => Some(-n),
        _ => None,
    }
}

/// What the shell persists in its session list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub agent: AgentKind,
    /// The agent's ACP session id, once the session exists.
    pub session_id: Option<String>,
    pub title: String,
    pub cwd: PathBuf,
    pub created: u64,
    pub updated: u64,
    /// The GitHub pull request the session opened or last pushed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    /// Claude Code: the seat (`seats`) the session runs on, kept for its
    /// whole life so a resumed session goes on with the same account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    /// The [`seats::fingerprint`] of that seat's token when the session last
    /// started on it, never the token: a different one under the same name
    /// may be another account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text {
        text: String,
    },
    Diff {
        path: PathBuf,
        old_text: Option<String>,
        new_text: String,
    },
    Terminal {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// ACP tool kind: read, edit, delete, move, search, execute, think, fetch, switch_mode, other.
    pub kind: String,
    pub status: ToolStatus,
    #[serde(default)]
    pub content: Vec<ToolContent>,
    #[serde(default)]
    pub locations: Vec<PathBuf>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<String>,
    /// Lines added/removed by this call's diffs.
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub removed: u32,
    /// The end of what a command printed, sent through ACP's
    /// `_meta.terminal_output` extension. Only the tail is kept: a noisy build
    /// would otherwise grow it, the transcript and every redraw without end.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal_output: String,
    /// Lines cut from the start of `terminal_output` to keep it short.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub terminal_dropped_lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Claude Code: the Agent call whose subagent made this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Claude Code's own name for the tool: Agent, Bash, Read…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// How much of a command's output a tool call keeps. The card shows 40 lines,
/// and the phone, the log and loop detection read only the end too.
const TERMINAL_KEEP: usize = 64 * 1024;

impl ToolCall {
    /// Adds what a command printed, then drops whole lines from the start once
    /// the output is twice `TERMINAL_KEEP`, so the cost of cutting is spread
    /// over many appends.
    pub fn push_terminal_output(&mut self, data: &str) {
        self.terminal_output.push_str(data);
        let len = self.terminal_output.len();
        if len <= 2 * TERMINAL_KEEP {
            return;
        }
        let mut cut = len - TERMINAL_KEEP;
        while !self.terminal_output.is_char_boundary(cut) {
            cut += 1;
        }
        // Start at a line so an escape sequence isn't cut in half. One line
        // longer than the whole tail is cut where it is.
        // Not when that newline ends the output: the tail would be empty.
        if let Some(nl) = self.terminal_output[cut..].find('\n')
            && cut + nl + 1 < len
        {
            cut += nl + 1;
        }
        // A line cut through the middle still counts once, by its kept end.
        let dropped = self.terminal_output[..cut].bytes().filter(|&b| b == b'\n');
        self.terminal_dropped_lines += dropped.count() as u64;
        self.terminal_output.drain(..cut);
    }

    /// A call that runs a subagent (Claude's Agent/Task, Droid's Task).
    pub fn is_subagent(&self) -> bool {
        matches!(self.tool_name.as_deref(), Some("Agent" | "Task"))
            || self.title.starts_with("Task")
            || self
                .raw_input
                .as_ref()
                .is_some_and(|i| i.get("subagent_type").is_some())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub priority: String,
    pub status: PlanStatus,
}

/// Where a plan entry stands. Stored in transcripts and sent by agents as
/// snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
    /// A status this version does not know; shown like `Pending`.
    #[serde(other)]
    Unknown,
}

impl PlanStatus {
    /// Reads a status the agent wrote as plain text, like Droid's
    /// "[in_progress] Do x".
    pub fn parse(s: &str) -> Self {
        parse_snake(s).unwrap_or(Self::Unknown)
    }
}

/// A snake_case string as one of the enums above, through their serde names
/// so the names live in one place.
fn parse_snake<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(Value::from(s)).ok()
}

/// How a notice from arakun shows besides its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeMark {
    /// Still going (a spinner); `update_notice` settles it.
    Busy,
    Passed,
}

/// A line of a file a notice points at, like an error a check printed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticePlace {
    pub path: String,
    pub line: u32,
    pub message: String,
}

// Tool calls are most of a transcript; boxing them would only add a hop.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    User {
        text: String,
        /// Unix seconds when it was sent.
        at: u64,
        /// How long the turn it started took, once it settled.
        #[serde(default)]
        elapsed_ms: Option<u64>,
        /// What rewinding refers to: Droid's id for the message; for ACP the
        /// agent's id for the message before it ("" when it is the first).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// Tokens the turn used, once it settled and when the agent reports usage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<u64>,
        /// Files the turn's shell commands changed that its edit tools didn't.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        shell_changes: Vec<ShellChange>,
    },
    Agent {
        text: String,
    },
    Thought {
        text: String,
    },
    Tool(ToolCall),
    Plan {
        entries: Vec<PlanEntry>,
    },
    Notice {
        text: String,
        #[serde(default)]
        error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mark: Option<NoticeMark>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        places: Vec<NoticePlace>,
    },
    /// The conversation was summarized to free context; `before`/`after` are
    /// the context used, in percent, when known.
    Compacted {
        #[serde(default)]
        removed: u64,
        #[serde(default)]
        before: Option<u64>,
        #[serde(default)]
        after: Option<u64>,
    },
}

#[derive(Clone, Debug)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    /// allow_once, allow_always, reject_once or reject_always.
    pub kind: String,
}

impl PermissionOption {
    pub fn is_allow(&self) -> bool {
        self.kind.starts_with("allow")
    }
}

#[derive(Clone, Debug)]
pub struct PermissionRequest {
    rpc_id: Value,
    pub tool_call_id: String,
    pub title: String,
    pub options: Vec<PermissionOption>,
    /// Set when native Droid asks to leave spec mode with this plan.
    pub plan: Option<SpecPlan>,
    /// The subagent session asking, when it is not this one.
    pub session: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// A session config option (ACP `configOptions`): mode, model, effort, ...
#[derive(Clone, Debug)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub current: String,
    pub options: Vec<Choice>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// Tokens in the context window (ACP `usage_update`; native Droid: its
    /// compaction meter, see `native::compaction_meter`).
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
    /// Session cost so far, with its currency.
    pub cost: Option<(f64, String)>,
    /// The cost is what the tokens would cost at API prices, not a charge:
    /// Claude Code signed in to a claude.ai plan.
    pub cost_estimate: bool,
    /// ACP: summed over prompt responses that report usage. Native Droid:
    /// the session totals, subagents included.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub thinking_tokens: u64,
    /// Factory Standard Credits the session has used (native Droid only).
    pub credits: Option<u64>,
    /// The model the last reported usage belongs to.
    pub model: Option<String>,
    /// The claude.ai plan limit Claude Code last warned about.
    #[serde(skip)]
    pub limit: Option<PlanLimit>,
}

/// A claude.ai plan window (5 hours, a week) as a `rate_limit_event` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanLimit {
    pub window: LimitWindow,
    pub status: LimitStatus,
    pub used_percent: Option<u64>,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

/// Which plan window a [`PlanLimit`] is about (`rateLimitType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitWindow {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    Overage,
    SevenDayOverageIncluded,
    #[serde(other)]
    Unknown,
}

/// How close a [`PlanLimit`] is (`status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitStatus {
    Allowed,
    AllowedWarning,
    Rejected,
    #[serde(other)]
    Unknown,
}

impl LimitStatus {
    pub fn parse(s: &str) -> Self {
        parse_snake(s).unwrap_or(Self::Unknown)
    }
}

impl PlanLimit {
    fn from_json(v: &Value) -> Option<Self> {
        let status = LimitStatus::parse(v["status"].as_str()?);
        // Claude Code reports a fraction; the /usage endpoint a percentage.
        let used_percent = v["utilization"]
            .as_f64()
            .filter(|u| u.is_finite() && *u >= 0.)
            .map(|u| if u <= 1. { u * 100. } else { u }.round() as u64);
        Some(PlanLimit {
            window: v["rateLimitType"]
                .as_str()
                .and_then(parse_snake)
                .unwrap_or(LimitWindow::FiveHour),
            status,
            used_percent,
            resets_at: v["resetsAt"].as_f64().filter(|t| *t > 0.).map(|t| t as u64),
        })
    }

    fn window_label(&self) -> &str {
        match self.window {
            LimitWindow::FiveHour => "5h limit",
            LimitWindow::SevenDay => "weekly limit",
            LimitWindow::SevenDayOpus => "Opus weekly limit",
            LimitWindow::SevenDaySonnet => "Sonnet weekly limit",
            LimitWindow::Overage | LimitWindow::SevenDayOverageIncluded => "extra usage",
            LimitWindow::Unknown => "plan limit",
        }
    }

    /// "5h limit 82%", "5h limit reached · resets 15:40"; None while nothing is near.
    pub fn label(&self) -> Option<String> {
        let name = self.window_label();
        if self.status == LimitStatus::Rejected {
            let reset = self
                .resets_at
                .and_then(crate::platform::local_time)
                .map(|t| format!(" · resets {:02}:{:02}", t.hour, t.minute))
                .unwrap_or_default();
            return Some(format!("{name} reached{reset}"));
        }
        match self.used_percent {
            Some(p) => Some(format!("{name} {p}%")),
            None if self.status == LimitStatus::AllowedWarning => Some(format!("{name} near")),
            None => None,
        }
    }
}

/// "$0.42", "≈ $0.42" for an estimate, "<$0.01", "1.20 EUR".
pub fn fmt_cost(amount: f64, currency: &str, estimate: bool) -> String {
    let approx = if estimate { "≈ " } else { "" };
    if currency.eq_ignore_ascii_case("USD") {
        if amount > 0. && amount < 0.01 {
            return format!("{approx}<$0.01");
        }
        return format!("{approx}${amount:.2}");
    }
    format!("{approx}{amount:.2} {currency}")
}

impl Usage {
    /// New tokens in and out. Cache reads are left out: every model call re-reads
    /// the whole cached context, so counting them mostly measures the number of calls.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_write_tokens
    }

    pub fn context_percent(&self) -> Option<u64> {
        match (self.context_used, self.context_size) {
            (Some(u), Some(s)) if s > 0 => Some((u * 100 + s / 2) / s),
            _ => None,
        }
    }

    /// The footer line, e.g. "12.3k tok · 4.1k credits · 15% context".
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        let tokens = self.total_tokens();
        if tokens > 0 {
            parts.push(format!("{} tok", fmt_count(tokens)));
        }
        if let Some(c) = self.credits {
            parts.push(format!("{} credits", fmt_count(c)));
        } else if let Some((amount, currency)) = &self.cost {
            parts.push(fmt_cost(*amount, currency, self.cost_estimate));
        }
        if let Some(p) = self.context_percent() {
            parts.push(format!("{p}% context"));
        }
        if let Some(l) = self.limit.as_ref().and_then(PlanLimit::label) {
            parts.push(l);
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[derive(Clone, Debug)]
pub enum ThreadEvent {
    /// Entries from this index on changed (an index equal to the old length means appended).
    EntriesChanged {
        from: usize,
    },
    /// The whole history was replaced (a transcript was loaded).
    Reset,
    /// Metadata changed: modes, models, config, queue, usage, commands.
    Changed,
    /// The agent's slash commands changed (also a [`ThreadEvent::Changed`]).
    CommandsChanged,
    StatusChanged,
    TitleChanged,
    NeedsPermission,
    /// A prompt turn ended; `stop_reason` is ACP's (end_turn, cancelled, ...) or "error".
    Finished {
        stop_reason: String,
    },
    /// `Thread::fork` made a copy of the session under a new id.
    Forked(SessionMeta),
    /// `Thread::confirm_rewind` forked the session before `prompt`, which goes
    /// back to the composer.
    Rewound {
        meta: SessionMeta,
        prompt: String,
    },
    /// The user asked to sign in to the agent (see [`Thread::request_login`]).
    LoginRequested,
    /// The rewind confirmation under the prompt at `entry` appeared, changed or went away.
    RewindChanged {
        entry: usize,
    },
    /// A settled turn's shell changes are known (see [`Thread::shell_pending`]).
    ShellChanges,
    /// The user answered the offer to use `command` as the project's check.
    CheckOffer {
        command: String,
        answer: CheckAnswer,
    },
    /// A subagent started (its id) or changed state (see [`Thread::subagents`]).
    SubagentsChanged {
        spawned: Option<String>,
    },
    /// What the user did in a session that runs on a server (see
    /// [`Thread::set_remote`]): it is for the server to carry out.
    Remote(RemoteAction),
}

/// Something the user did in a server's session, which goes to the server.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteAction {
    /// `now` stops the running turn for the prompt instead of queueing it.
    Send {
        text: String,
        now: bool,
    },
    /// Stops the running turn; the queue there pauses, as it does here.
    Cancel,
    /// Takes the queued prompt `text`, seen at `index`, out of the queue:
    /// `now` sends it once the running turn stopped, otherwise it is dropped.
    Unqueue {
        index: usize,
        text: String,
        now: bool,
    },
    /// Lets the paused queue go again.
    ResumeQueue,
    /// The option picked for the permission asked.
    Permission(String),
    Answer(Vec<String>),
    SkipQuestion,
    /// Copy the session into a new one there.
    Fork,
    /// Summarize the conversation so far, to free context.
    Compact,
    /// What rewinding to before the prompt at this entry would undo.
    RewindPlan(usize),
    /// Rewind to before the prompt at this entry, as the plan shown said.
    Rewind(usize),
    /// Change the model or reasoning the session is on.
    Setting(Setting, String),
}

/// What the user made of the offer to set up a check (see [`Thread::offer_check`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckAnswer {
    Use,
    Edit,
    Never,
}

#[derive(Serialize, Deserialize)]
pub struct Transcript {
    pub version: u32,
    pub meta: SessionMeta,
    pub entries: Vec<Entry>,
    /// What the session used so far, for agents that do not report totals
    /// again when a session is reopened (Claude Code).
    #[serde(default)]
    pub usage: Usage,
    /// ACP: the agent's id for its last message (see `Thread::fork_point`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_point: Option<String>,
    /// A turn was still running when this was saved: arakun closed under it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Saved by `arakun restart`: the interrupted turn goes on by itself on reopen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
    /// Prompts still waiting their turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<String>,
    /// The conversation so far, for an agent continuing another's session; it
    /// goes out with the next prompt and is never shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
    /// Subagents that ran as child sessions, with their own transcripts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<SubagentRecord>,
}

/// A saved transcript made ready to show by [`Thread::read_restored`].
pub struct Restored(Transcript);

/// What a session interrupted by `arakun restart` is told when it comes back.
const RESTARTED: &str =
    "Arakun restarted while you were working (a new build). Continue where you left off.";

#[derive(Default)]
struct Caps {
    load: bool,
    resume: bool,
    image: bool,
    /// `session/fork`.
    fork: bool,
    /// The AIR `_session/rename` extension.
    rename: bool,
}

pub struct Thread {
    kind: AgentKind,
    backend: Backend,
    /// The sandbox the agent runs in, when its project has one; its `fs/*`
    /// requests follow the same rules.
    sandbox: Option<Arc<super::sandbox::Session>>,
    /// The session runs on a server: this thread shows it, and what the user
    /// does goes out as [`ThreadEvent::Remote`].
    remote: bool,
    /// Copies of the images a server's session names, by link as written:
    /// the files are on the server, not here.
    fetched_images: HashMap<String, PathBuf>,
    native: native::State,
    cwd: PathBuf,
    meta: SessionMeta,
    status: ThreadStatus,
    status_detail: Option<SharedString>,
    entries: Vec<Entry>,
    tool_index: HashMap<String, usize>,
    /// What files looked like before the running tool calls that rewrite them.
    replaced: HashMap<(String, PathBuf), Option<String>>,
    last_message_id: Option<String>,
    conn: Option<Rc<Connection>>,
    /// The seat the running agent process is on, counted as live there.
    lease: Option<seats::Lease>,
    session_id: Option<String>,
    agent_title: Option<String>,
    caps: Caps,
    auth_hint: Option<String>,
    /// The last failure was the agent saying it is not signed in.
    needs_login: bool,
    /// The last failure was an adapter that is not installed yet.
    needs_install: Option<launch::AdapterInstall>,
    /// The user agreed to install the agent's adapter.
    install_approved: bool,
    modes: Option<(String, Vec<Choice>)>,
    models: Option<(String, Vec<Choice>)>,
    config: Vec<ConfigOption>,
    commands: Vec<Choice>,
    /// The agent said which commands it has, even if none: an empty list is then the real one.
    commands_known: bool,
    permissions: VecDeque<PermissionRequest>,
    queue: Vec<String>,
    queue_paused: bool,
    /// How many prompts at the front of the queue the user asked to send now: they
    /// go once the interrupted turn stopped, even while the queue is paused.
    urgent: usize,
    usage: Usage,
    turn_started: Option<Instant>,
    /// When each running tool call was first seen, for its timer. Not saved:
    /// a call replayed from a transcript has no clock worth showing.
    tool_started: HashMap<String, Instant>,
    /// Session tokens when the turn started, to stamp the turn with what it used.
    turn_tokens: u64,
    /// Session usage when the turn started, for the usage log.
    turn_usage: Usage,
    /// What each tool's results used in the turn, when the agent says
    /// (arakun's Workers AI loop), for the usage log.
    turn_tools: BTreeMap<String, ledger::ToolUse>,
    /// The last cost Claude Code reported in this process; it counts from the
    /// agent's start, or from the saved total when it resumes a session.
    cost_seen: Option<f64>,
    /// Cost the agent's own count leaves out (it started over).
    cost_base: f64,
    /// ACP: the agent's id for its last message, where a rewind to the next
    /// prompt forks the session.
    fork_point: Option<String>,
    turn_user: Option<usize>,
    /// How long the last turn took, including one the agent took by itself.
    last_turn_ms: Option<u64>,
    last_prompt: Option<String>,
    cancelling: bool,
    /// ACP settings picked before the session existed, applied in order once it does.
    want_config: Vec<(Setting, String)>,
    /// Those settings are being applied; the first prompt waits for them.
    applying_config: bool,
    /// Set while `session/load` replays history we already have from a transcript.
    suppress_replay: bool,
    replaying: bool,
    /// "Retrying (2/10)…" while the model API is being retried; gone on output or turn end.
    retry_status: Option<String>,
    /// The last error the agent reported as a session failure, used to explain a failed turn.
    failure: Option<String>,
    /// Where the last failure happened, for crash reports.
    failed_at: Option<FailedAt>,
    /// Whether crash reports have yet to hear of [`Self::failed_at`].
    failure_unreported: bool,
    /// The id of the crash report sent for the current error, for the form
    /// that asks what happened.
    error_event: Option<String>,
    /// The running turn seems to be going in circles.
    stuck: Option<Stuck>,
    /// Patterns the user chose to keep going past this turn.
    stuck_dismissed: Vec<String>,
    /// Sent once the turn "Stop and rethink" cancelled has stopped.
    rethink: Option<String>,
    /// A check arakun offers to set up for a project that has none, until answered.
    check_offer: Option<String>,
    check_offered: bool,
    /// The checkout when the running turn started.
    turn_tree: Option<Shared<Task<Option<Snapshot>>>>,
    /// Working out what the last settled turn's shell commands changed.
    shell_pending: Option<Shared<Task<()>>>,
    title_locked: bool,
    /// Permission requests are answered with an allow option at once: the
    /// session runs on its own (`arakun agent new`).
    auto_allow: bool,
    /// See [`Transcript::handoff`]; dropped once a turn that carried it ends well.
    handoff: Option<String>,
    handoff_sent: bool,
    /// Context arakun adds to the next prompt the user sends, out of sight.
    notes: Vec<String>,
    /// Subagents in the order they started (see `subagents.rs`).
    subagents: Vec<Subagent>,
    /// The session running this one as a subagent; set on the read-only
    /// thread that holds a subagent's transcript.
    parent: Option<WeakEntity<Thread>>,
    /// A subagent's latest step, from the transcript Claude Code writes for
    /// it (see `subagent_trail.rs`); set on its read-only thread.
    trail: Option<super::subagent_trail::Activity>,
    /// Reads the transcripts of the running subagents while any runs.
    _trail_watch: Option<Task<()>>,
    /// Counts ACP prompts; a response to an older one than the last is stale.
    prompt_seq: u64,
    /// The last ACP prompt is unanswered.
    prompting: bool,
    /// The prompt's own work is done, but the agent holds its response until
    /// the background subagents it started finish: the session is the user's again.
    held: bool,
    /// The agent took a turn nobody prompted (a background subagent finished).
    auto_turn: bool,
    /// The agent says which turns a person started (Claude's `_claude/origin`),
    /// so a turn it takes on its own has a known end.
    origins: bool,
    _connect: Option<Task<()>>,
    _pump: Option<Task<()>>,
    _setup: Option<Task<()>>,
    _prompt: Option<Task<()>>,
}

impl EventEmitter<ThreadEvent> for Thread {}

fn summarize(text: &str) -> String {
    let text = attach::title_text(text);
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

/// What Claude's adapter appends to its system prompt: the window's commands
/// when there is a window, and always how to draw a picture in the chat.
fn claude_system_hint(cwd: &Path) -> String {
    match launch::agent_hint(Some(cwd)) {
        Some(hint) => format!("{hint}\n\n{}", super::chart::DRAW_NOTE),
        None => super::chart::DRAW_NOTE.to_string(),
    }
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn content_text(block: &Value) -> String {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => str_of(block, "text").unwrap_or_default(),
        Some("image") => "[image]".into(),
        Some("audio") => "[audio]".into(),
        Some("resource_link") => {
            let uri = str_of(block, "uri").unwrap_or_default();
            let name = str_of(block, "name").unwrap_or_else(|| uri.clone());
            format!("[{name}]({uri})")
        }
        Some("resource") => block
            .get("resource")
            .and_then(|r| str_of(r, "text").or_else(|| str_of(r, "uri")))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn parse_choices(list: Option<&Value>, id_key: &str) -> Vec<Choice> {
    list.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(Choice {
                        id: str_of(m, id_key).or_else(|| str_of(m, "value"))?,
                        name: str_of(m, "name").unwrap_or_default(),
                        description: str_of(m, "description"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ACP config option a composer setting is, if the agent offers it.
pub fn acp_option(config: &[ConfigOption], s: Setting) -> Option<&ConfigOption> {
    let category = s.acp_category()?;
    config.iter().find(|o| {
        o.category.as_deref() == Some(category)
            || o.id == category
            // Codex files fast mode under `model_config`, with this id.
            || (s == Setting::Fast && o.id == "fast-mode")
    })
}

/// An ACP config option in the shape the composer menus use.
pub fn acp_setting_options(o: &ConfigOption, s: Setting) -> (String, Vec<SettingOption>) {
    let options = o
        .options
        .iter()
        .map(|c| SettingOption {
            id: c.id.clone(),
            name: match s {
                Setting::Effort if c.id != "default" => effort_name(&c.id),
                _ => c.name.clone(),
            },
            detail: None,
            description: c.description.clone(),
            disabled: None,
        })
        .collect();
    (o.current.clone(), options)
}

/// The label a chip shows for an ACP setting. Claude's "Default (recommended)"
/// model reads as the model it stands for.
pub fn acp_label(s: Setting, current: &str, options: &[SettingOption]) -> String {
    let Some(o) = options.iter().find(|o| o.id == current) else {
        return current.to_string();
    };
    match s {
        Setting::Model if o.id == "default" => {
            o.description.clone().unwrap_or_else(|| o.name.clone())
        }
        _ => o.name.clone(),
    }
}

/// Puts each option of `update` in place of the one with its id in `config`,
/// adding the new ones and keeping the rest.
fn merge_config(config: &mut Vec<ConfigOption>, update: Vec<ConfigOption>) {
    for o in update {
        match config.iter_mut().find(|c| c.id == o.id) {
            Some(c) => *c = o,
            None => config.push(o),
        }
    }
}

pub fn parse_config(list: &Value) -> Vec<ConfigOption> {
    list.as_array()
        .map(|a| {
            a.iter()
                .filter(|o| {
                    o.get("type")
                        .and_then(Value::as_str)
                        .is_none_or(|t| t == "select")
                })
                .filter_map(|o| {
                    // Options may come grouped: [{group, name, options: [...]}].
                    let mut options = Vec::new();
                    for item in o
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(inner) = item.get("options") {
                            options.extend(parse_choices(Some(inner), "value"));
                        } else {
                            options.extend(parse_choices(Some(&json!([item])), "value"));
                        }
                    }
                    Some(ConfigOption {
                        id: str_of(o, "id")?,
                        name: str_of(o, "name").unwrap_or_default(),
                        category: str_of(o, "category"),
                        current: o
                            .get("currentValue")
                            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                            .unwrap_or_default(),
                        options,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_tool_status(s: &str) -> ToolStatus {
    match s {
        "in_progress" => ToolStatus::InProgress,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        _ => ToolStatus::Pending,
    }
}

fn parse_tool_content(list: &Value) -> Vec<ToolContent> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| match c.get("type").and_then(Value::as_str)? {
            "content" => {
                let text = content_text(c.get("content")?);
                Some(ToolContent::Text { text })
            }
            "diff" => Some(ToolContent::Diff {
                path: PathBuf::from(str_of(c, "path")?),
                old_text: str_of(c, "oldText"),
                new_text: str_of(c, "newText").unwrap_or_default(),
            }),
            "terminal" => Some(ToolContent::Terminal {
                id: str_of(c, "terminalId").unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

/// The option that allows a tool call, "allow once" first.
fn allow_option(options: &[PermissionOption]) -> Option<&PermissionOption> {
    options
        .iter()
        .find(|o| o.kind == "allow_once")
        .or_else(|| options.iter().find(|o| o.is_allow()))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Another agent's history, ready to sit above `kind`'s: nothing in it can be
/// rewound (those ids belong to the other agent), what was cut off stays
/// failed, and a line says who goes on.
fn continued_entries(mut entries: Vec<Entry>, kind: AgentKind) -> Vec<Entry> {
    for e in &mut entries {
        match e {
            Entry::User { message_id, .. } => *message_id = None,
            Entry::Tool(tool)
                if matches!(tool.status, ToolStatus::Pending | ToolStatus::InProgress) =>
            {
                tool.status = ToolStatus::Failed;
            }
            _ => {}
        }
    }
    entries.push(Entry::Notice {
        text: format!("Continued with {}", kind.label()),
        error: false,
        mark: None,
        places: Vec::new(),
    });
    entries
}

impl Thread {
    /// Starts `kind` in `cwd`. With `resume`, the agent's session is loaded
    /// (`session/load`/`session/resume`) when it supports that, else a new one is made.
    pub fn start(
        kind: AgentKind,
        cwd: PathBuf,
        resume: Option<SessionMeta>,
        cx: &mut App,
    ) -> Entity<Thread> {
        cx.new(|cx| {
            let mut this = Thread::new(kind, cwd, resume);
            this.connect(cx);
            this
        })
    }

    /// Reopens a saved transcript: its history shows at once, and the agent's
    /// session is resumed underneath without replaying it again.
    pub fn restore(path: &Path, cx: &mut App) -> Result<Entity<Thread>> {
        Ok(Self::restored(Self::read_restored(path)?, cx))
    }

    /// The slow half of [`Thread::restore`]: reads and parses the transcript,
    /// and rebuilds older diffs with git. It needs no `App`, so it can run off
    /// the main thread; [`Thread::restored`] then shows it.
    pub fn read_restored(path: &Path) -> Result<Restored> {
        let mut t = Self::read_transcript(path)?;
        let entries = &mut t.entries;
        fill_replaced_history(entries, &t.meta.cwd);
        if t.interrupted {
            // Tools cut off with the turn never finish now.
            for e in entries.iter_mut() {
                if let Entry::Tool(tool) = e
                    && matches!(tool.status, ToolStatus::Pending | ToolStatus::InProgress)
                {
                    tool.status = ToolStatus::Failed;
                }
            }
            if !t.resume {
                entries.push(Entry::Notice {
                    text:
                        "Arakun closed while this turn was running. Send a message to pick it up."
                            .into(),
                    error: false,
                    mark: None,
                    places: Vec::new(),
                });
            }
        }
        Ok(Restored(t))
    }

    /// Shows a transcript [`Thread::read_restored`] read, and resumes its agent.
    pub fn restored(Restored(t): Restored, cx: &mut App) -> Entity<Thread> {
        cx.new(|cx| {
            let mut this = Thread::new(t.meta.agent, t.meta.cwd.clone(), Some(t.meta));
            this.set_entries(t.entries);
            this.load_subagents(t.subagents, true, cx);
            this.restore_usage(t.usage);
            this.fork_point = t.fork_point;
            this.queue = t.queue;
            this.handoff = t.handoff;
            if t.interrupted && t.resume {
                this.queue.insert(0, RESTARTED.into());
            } else {
                // After an unplanned close, waiting prompts wait for the user.
                this.queue_paused = !this.queue.is_empty();
            }
            this.connect(cx);
            this
        })
    }

    /// `kind` taking over a conversation another agent had in `cwd`: `earlier`
    /// shows as its history, and `handoff` goes out with the next prompt.
    pub fn continued(
        kind: AgentKind,
        cwd: PathBuf,
        title: String,
        earlier: Vec<Entry>,
        handoff: String,
        cx: &mut App,
    ) -> Entity<Thread> {
        cx.new(|cx| {
            let mut this = Thread::new(kind, cwd.clone(), None);
            if !title.is_empty() {
                this.meta.title = title;
                this.title_locked = true;
            }
            let mut entries = continued_entries(earlier, kind);
            fill_replaced_history(&mut entries, &cwd);
            this.set_entries(entries);
            this.load_subagents(Vec::new(), true, cx);
            this.handoff = Some(handoff);
            this.connect(cx);
            this
        })
    }

    /// A transcript shown with no agent behind it: a session another process
    /// runs (`arakun agent run`). [`Thread::show_transcript`] follows it.
    pub fn viewer(t: Transcript, status: ThreadStatus, cx: &mut App) -> Entity<Thread> {
        cx.new(|cx| {
            let mut this = Thread::new(t.meta.agent, t.meta.cwd.clone(), Some(t.meta.clone()));
            this.show_transcript(t, status, cx);
            this
        })
    }

    /// Takes the transcript as it is now; only what changed near its end is redrawn.
    pub fn show_transcript(&mut self, t: Transcript, status: ThreadStatus, cx: &mut Context<Self>) {
        const WINDOW: usize = 64;
        let old = self.entries.len();
        let start = old.saturating_sub(WINDOW).min(t.entries.len());
        let from = (start..old.min(t.entries.len()))
            .find(|&i| self.entries[i] != t.entries[i])
            .unwrap_or(old.min(t.entries.len()));
        if !t.meta.title.is_empty() && t.meta.title != self.meta.title {
            self.meta.title = t.meta.title.clone();
            self.title_locked = true;
            cx.emit(ThreadEvent::TitleChanged);
        }
        self.session_id = t.meta.session_id.clone();
        self.meta = t.meta;
        self.usage = t.usage;
        self.queue = t.queue;
        self.urgent = self.urgent.min(self.queue.len());
        let shrank = t.entries.len() < old;
        if shrank || from < t.entries.len() {
            self.set_entries(t.entries);
            if shrank {
                cx.emit(ThreadEvent::Reset);
            } else {
                cx.emit(ThreadEvent::EntriesChanged { from });
            }
        }
        self.load_subagents(t.subagents, false, cx);
        self.status_detail = None;
        self.set_status(status, cx);
        self.meta_changed(cx);
    }

    fn new(kind: AgentKind, cwd: PathBuf, resume: Option<SessionMeta>) -> Self {
        let now = unix_now();
        let meta = resume.unwrap_or_else(|| SessionMeta {
            agent: kind,
            session_id: None,
            title: String::new(),
            cwd: cwd.clone(),
            created: now,
            updated: now,
            pr: None,
            seat: None,
            seat_key: None,
        });
        let backend = if kind == AgentKind::Droid && launch::droid_native() {
            Backend::Native
        } else {
            Backend::Acp
        };
        Thread {
            kind,
            backend,
            remote: false,
            fetched_images: HashMap::new(),
            native: native::State::default(),
            session_id: meta.session_id.clone(),
            title_locked: !meta.title.is_empty(),
            meta,
            cwd,
            status: ThreadStatus::Connecting,
            status_detail: None,
            entries: Vec::new(),
            tool_index: HashMap::new(),
            replaced: HashMap::new(),
            last_message_id: None,
            conn: None,
            lease: None,
            agent_title: None,
            caps: Caps::default(),
            auth_hint: None,
            needs_login: false,
            needs_install: None,
            sandbox: None,
            install_approved: false,
            modes: None,
            models: None,
            config: Vec::new(),
            commands: Vec::new(),
            commands_known: false,
            permissions: VecDeque::new(),
            queue: Vec::new(),
            queue_paused: false,
            urgent: 0,
            usage: Usage {
                cost_estimate: kind == AgentKind::Claude
                    && std::env::var_os("ANTHROPIC_API_KEY").is_none(),
                ..Usage::default()
            },
            turn_started: None,
            tool_started: HashMap::new(),
            turn_tokens: 0,
            turn_usage: Usage::default(),
            turn_tools: BTreeMap::new(),
            cost_seen: None,
            cost_base: 0.,
            fork_point: None,
            turn_user: None,
            last_turn_ms: None,
            last_prompt: None,
            cancelling: false,
            want_config: Vec::new(),
            applying_config: false,
            suppress_replay: false,
            replaying: false,
            retry_status: None,
            stuck: None,
            stuck_dismissed: Vec::new(),
            rethink: None,
            check_offer: None,
            check_offered: false,
            turn_tree: None,
            shell_pending: None,
            failure: None,
            failed_at: None,
            failure_unreported: false,
            error_event: None,
            auto_allow: false,
            handoff: None,
            handoff_sent: false,
            notes: Vec::new(),
            subagents: Vec::new(),
            parent: None,
            trail: None,
            _trail_watch: None,
            prompt_seq: 0,
            prompting: false,
            held: false,
            auto_turn: false,
            origins: false,
            _connect: None,
            _pump: None,
            _setup: None,
            _prompt: None,
        }
    }

    // ---- accessors -------------------------------------------------------

    pub fn kind(&self) -> AgentKind {
        self.kind
    }

    pub fn last_turn_ms(&self) -> Option<u64> {
        self.last_turn_ms
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn status(&self) -> ThreadStatus {
        self.status.clone()
    }

    /// A short line about what is happening while connecting ("Installing…").
    pub fn status_detail(&self) -> Option<SharedString> {
        self.status_detail.clone()
    }

    pub fn retry_status(&self) -> Option<&str> {
        self.retry_status.as_deref()
    }

    pub(super) fn set_retry(&mut self, status: Option<String>, cx: &mut Context<Self>) {
        if self.retry_status != status {
            self.retry_status = status;
            cx.notify();
        }
    }

    pub fn title(&self) -> SharedString {
        if self.meta.title.is_empty() {
            "New session".into()
        } else {
            self.meta.title.clone().into()
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The agent's own name and version from `initialize`, e.g. "Factory Droid 0.240.1".
    pub fn agent_title(&self) -> Option<&str> {
        self.agent_title.as_deref()
    }

    pub fn supports_load(&self) -> bool {
        self.caps.load
    }

    pub fn pending_permission(&self) -> Option<&PermissionRequest> {
        self.permissions.front()
    }

    pub fn queued(&self) -> &[String] {
        &self.queue
    }

    pub fn queue_paused(&self) -> bool {
        self.queue_paused
    }

    /// How many queued prompts go first once the interrupted turn stopped.
    pub fn sending_now(&self) -> usize {
        self.urgent
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancelling
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.status,
            ThreadStatus::Working | ThreadStatus::NeedsInput
        )
    }

    /// Starts a call's clock when it first runs and drops it when it ends.
    fn clock_tool(&mut self, id: &str, status: ToolStatus) {
        match status {
            ToolStatus::Completed | ToolStatus::Failed => {
                self.tool_started.remove(id);
            }
            _ if !self.replaying => {
                self.tool_started
                    .entry(id.to_string())
                    .or_insert_with(Instant::now);
            }
            _ => {}
        }
    }

    /// How long a running tool call has been going.
    pub fn tool_elapsed(&self, id: &str) -> Option<Duration> {
        self.tool_started.get(id).map(Instant::elapsed)
    }

    /// How long the running turn has been going.
    pub fn turn_elapsed(&self) -> Option<Duration> {
        self.turn_started.map(|t| t.elapsed())
    }

    /// Index of the user entry that opened the running turn.
    pub fn turn_entry(&self) -> Option<usize> {
        self.is_busy().then_some(self.turn_user).flatten()
    }

    /// Current mode and the available ones (Claude: Manual/Accept edits/Plan/...;
    /// Droid: Auto (Off/Low/Medium/High), Spec).
    pub fn modes(&self) -> Option<(String, Vec<Choice>)> {
        if self.backend == Backend::Native {
            return self.setting_choices(Setting::Mode);
        }
        if let Some(o) = self.config_by_category("mode") {
            return Some((o.current.clone(), o.options.clone()));
        }
        self.modes.clone()
    }

    pub fn models(&self) -> Option<(String, Vec<Choice>)> {
        if self.backend == Backend::Native {
            return self.setting_choices(Setting::Model);
        }
        if let Some(o) = self.config_by_category("model") {
            return Some((o.current.clone(), o.options.clone()));
        }
        self.models.clone()
    }

    pub fn config_options(&self) -> &[ConfigOption] {
        &self.config
    }

    /// Slash commands the agent advertised.
    pub fn commands(&self) -> &[Choice] {
        &self.commands
    }

    /// Whether the agent has said which slash commands it has yet.
    pub fn commands_known(&self) -> bool {
        self.commands_known
    }

    /// The slash commands of a server's session, as the server sent them:
    /// a viewer has no agent of its own to advertise them.
    pub fn set_remote_commands(&mut self, commands: Vec<Choice>, cx: &mut Context<Self>) {
        self.commands = commands;
        self.commands_known = true;
        self.meta_changed(cx);
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Files this session's finished edits and shell commands touched,
    /// absolute, in first-touched order.
    pub fn edited_files(&self) -> Vec<PathBuf> {
        edited_files(&self.entries, &self.cwd)
    }

    /// A line in the transcript from arakun itself; returns its index for `update_notice`.
    pub fn post_notice(
        &mut self,
        text: impl Into<String>,
        error: bool,
        cx: &mut Context<Self>,
    ) -> usize {
        self.post_marked_notice(text, error, None, cx)
    }

    pub fn post_marked_notice(
        &mut self,
        text: impl Into<String>,
        error: bool,
        mark: Option<NoticeMark>,
        cx: &mut Context<Self>,
    ) -> usize {
        self.push_entry(
            Entry::Notice {
                text: text.into(),
                error,
                mark,
                places: Vec::new(),
            },
            cx,
        )
    }

    /// Rewrites the notice at `ix`; posts a new one if the history changed under it.
    pub fn update_notice(
        &mut self,
        ix: usize,
        text: impl Into<String>,
        error: bool,
        mark: Option<NoticeMark>,
        places: Vec<NoticePlace>,
        cx: &mut Context<Self>,
    ) {
        let notice = Entry::Notice {
            text: text.into(),
            error,
            mark,
            places,
        };
        match self.entries.get_mut(ix) {
            Some(
                entry @ Entry::Notice {
                    mark: Some(NoticeMark::Busy),
                    ..
                },
            ) => {
                *entry = notice;
                self.entries_changed(ix, cx);
            }
            _ => {
                self.push_entry(notice, cx);
            }
        }
    }

    /// Whether the latest turn changed files with its edit tools.
    pub fn turn_edited(&self) -> bool {
        let start = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::User { .. }))
            .map_or(0, |i| i + 1);
        self.entries[start..].iter().any(|e| {
            matches!(e, Entry::Tool(t) if t.status == ToolStatus::Completed
                && (matches!(t.kind.as_str(), "edit" | "delete" | "move")
                    || t.content.iter().any(|c| matches!(c, ToolContent::Diff { .. }))))
        })
    }

    /// When the latest turn was sent, if it ran a shell command, which may
    /// have changed files its edit tools never saw.
    pub fn turn_shell_since(&self) -> Option<u64> {
        let start = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::User { .. }))?;
        let Entry::User { at, .. } = &self.entries[start] else {
            return None;
        };
        self.entries[start + 1..]
            .iter()
            .any(|e| matches!(e, Entry::Tool(t) if t.kind == "execute"))
            .then_some(*at)
    }

    /// Lines added and removed across every edit this session made.
    pub fn diff_stats(&self) -> (u32, u32) {
        self.entries.iter().fold((0, 0), |(a, r), e| match e {
            Entry::Tool(t) if t.status == ToolStatus::Completed => (a + t.added, r + t.removed),
            Entry::User { shell_changes, .. } => shell_changes
                .iter()
                .fold((a, r), |(a, r), c| (a + c.added, r + c.removed)),
            _ => (a, r),
        })
    }

    pub fn meta(&self) -> SessionMeta {
        let mut meta = self.meta.clone();
        meta.session_id = self.session_id.clone();
        meta
    }

    /// A native setting in the shape ACP modes and models use, minus disabled choices.
    fn setting_choices(&self, s: Setting) -> Option<(String, Vec<Choice>)> {
        let (current, options) = self.setting(s)?;
        let choices = options
            .into_iter()
            .filter(|o| o.disabled.is_none())
            .map(|o| Choice {
                id: o.id,
                name: o.name,
                description: o.detail,
            })
            .collect();
        Some((current, choices))
    }

    /// The ACP setting's value and choices, from the agent's config options (or
    /// its older modes and models lists).
    fn acp_setting(&self, s: Setting) -> Option<(String, Vec<SettingOption>)> {
        if let Some(o) = acp_option(&self.config, s) {
            return Some(acp_setting_options(o, s));
        }
        let (current, choices) = match s {
            Setting::Mode => self.modes.as_ref()?,
            Setting::Model => self.models.as_ref()?,
            _ => return None,
        };
        let option = ConfigOption {
            id: String::new(),
            name: String::new(),
            category: None,
            current: current.clone(),
            options: choices.clone(),
        };
        Some(acp_setting_options(&option, s))
    }

    fn acp_setting_label(&self, s: Setting) -> Option<String> {
        let (current, options) = self.acp_setting(s)?;
        Some(acp_label(s, &current, &options))
    }

    fn config_by_category(&self, category: &str) -> Option<&ConfigOption> {
        self.config
            .iter()
            .find(|o| o.category.as_deref() == Some(category) || o.id == category)
    }

    /// Kills the agent now instead of when the entity is dropped.
    pub fn shutdown(&mut self) {
        self._setup = None;
        self._prompt = None;
        self._pump = None;
        self._connect = None;
        self.conn = None;
        self.lease = None;
    }

    // ---- persistence -----------------------------------------------------

    pub fn transcript(&self, cx: &App) -> Transcript {
        Transcript {
            version: 1,
            meta: self.meta(),
            entries: self.entries.clone(),
            usage: self.usage.clone(),
            fork_point: self.fork_point.clone(),
            interrupted: self.is_busy(),
            resume: false,
            queue: self.queue.clone(),
            handoff: self.handoff.clone(),
            subagents: self.subagent_records(cx),
        }
    }

    /// Takes the totals a saved transcript had, unless the session already counts its own.
    fn restore_usage(&mut self, saved: Usage) {
        if self.usage.total_tokens() > 0
            || self.usage.cost.is_some()
            || self.usage.credits.is_some()
        {
            return;
        }
        let estimate = self.usage.cost_estimate;
        self.usage = Usage {
            cost_estimate: estimate,
            limit: None,
            ..saved
        };
    }

    /// Written so only the owner can read it: it holds the whole chat.
    pub fn save_transcript(&self, path: &Path, cx: &App) -> Result<()> {
        crate::durable::write_private(path, &serde_json::to_vec(&self.transcript(cx))?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn read_transcript(path: &Path) -> Result<Transcript> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Replaces the history with a saved transcript. Call right after [`Thread::start`]
    /// (or use [`Thread::restore`]); a later `session/load` replay is then ignored.
    pub fn load_transcript(&mut self, path: &Path, cx: &mut Context<Self>) -> Result<()> {
        let t = Self::read_transcript(path)?;
        if self.meta.title.is_empty() {
            self.meta.title = t.meta.title;
            self.title_locked = !self.meta.title.is_empty();
            cx.emit(ThreadEvent::TitleChanged);
        }
        self.meta.created = self.meta.created.min(t.meta.created);
        let mut entries = t.entries;
        fill_replaced_history(&mut entries, &self.cwd);
        self.set_entries(entries);
        self.load_subagents(t.subagents, true, cx);
        self.restore_usage(t.usage);
        self.fork_point = self.fork_point.take().or(t.fork_point);
        cx.emit(ThreadEvent::Reset);
        cx.notify();
        Ok(())
    }

    fn set_entries(&mut self, mut entries: Vec<Entry>) {
        // Whatever was still going when the transcript was saved is not anymore.
        for e in &mut entries {
            if let Entry::Notice {
                mark: mark @ Some(NoticeMark::Busy),
                ..
            } = e
            {
                *mark = None;
            }
        }
        self.entries = entries;
        self.native.blocks.clear();
        self.tool_index = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Entry::Tool(t) => Some((t.id.clone(), i)),
                _ => None,
            })
            .collect();
        self.suppress_replay = !self.entries.is_empty();
    }

    // ---- change plumbing -------------------------------------------------

    fn entries_changed(&mut self, from: usize, cx: &mut Context<Self>) {
        self.meta.updated = unix_now();
        if self.turn_started.is_some()
            && let Some(Entry::Tool(t)) = self.entries.get(from)
            && matches!(t.status, ToolStatus::Completed | ToolStatus::Failed)
        {
            let turn = self.turn_user.map_or(0, |ix| ix + 1);
            self.stuck = loops::detect(
                self.entries.get(turn..).unwrap_or(&[]),
                &self.stuck_dismissed,
            );
        }
        cx.emit(ThreadEvent::EntriesChanged { from });
        cx.notify();
    }

    fn meta_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(ThreadEvent::Changed);
        cx.notify();
    }

    fn set_status(&mut self, status: ThreadStatus, cx: &mut Context<Self>) {
        if status == ThreadStatus::Idle && self.handoff_sent {
            self.handoff = None;
            self.handoff_sent = false;
        }
        if self.status != status {
            self.status = status;
            cx.emit(ThreadEvent::StatusChanged);
            cx.notify();
        }
    }

    fn push_entry(&mut self, entry: Entry, cx: &mut Context<Self>) -> usize {
        let ix = self.entries.len();
        if let Entry::Tool(t) = &entry {
            self.tool_index.insert(t.id.clone(), ix);
        }
        self.entries.push(entry);
        self.entries_changed(ix, cx);
        ix
    }

    fn notice(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.post_marked_notice(text, error, None, cx);
    }

    /// Notes a new failure. The report of an earlier one is not this one's, so
    /// feedback must not link it.
    fn record_failure(&mut self, at: FailedAt) {
        self.status_detail = None;
        self.failed_at = Some(at);
        self.failure_unreported = true;
        self.error_event = None;
    }

    fn fail(&mut self, at: FailedAt, message: String, cx: &mut Context<Self>) {
        self.record_failure(at);
        self.set_status(ThreadStatus::Error(message), cx);
    }

    // ---- connection ------------------------------------------------------

    fn connect(&mut self, cx: &mut Context<Self>) {
        self.conn = None;
        self.lease = None;
        self._pump = None;
        self._setup = None;
        self._prompt = None;
        (self.prompting, self.held, self.auto_turn) = (false, false, false);
        self.permissions.clear();
        self.native.questions.clear();
        self.needs_login = false;
        self.needs_install = None;
        self.status_detail = Some(format!("Starting {}…", self.kind.label()).into());
        self.set_status(ThreadStatus::Connecting, cx);
        let kind = self.kind;
        let native = self.backend == Backend::Native;
        let cwd = self.cwd.clone();
        let install = self.take_install_approval();
        let pinned = self.meta.seat.clone();
        let pinned_key = self.meta.seat_key.clone();
        self._connect = Some(cx.spawn(async move |this, cx| {
            let (progress_tx, mut progress_rx) = fmpsc::unbounded::<String>();
            let resolve = cx.background_executor().spawn(async move {
                if native {
                    return launch::resolve_droid_native(Some(&cwd)).map(|s| (s, None, None));
                }
                let mut spec = launch::resolve(kind, Some(&cwd), install, &|line| {
                    let _ = progress_tx.unbounded_send(line);
                })?;
                let sandboxed = super::sandbox::wanted(&cwd).map_err(|e| {
                    anyhow!("{e}. The sandbox won't start an agent in a Git folder it can't trust.")
                })?;
                // Each Claude Code session on a seat of its own, in its own
                // environment only; none in the sandbox. See `seats`.
                let seat = (kind == AgentKind::Claude).then(|| {
                    seats::assign(pinned.as_deref(), pinned_key.as_deref(), sandboxed)
                });
                if let Some(seats::Seating::On(seat)) = &seat {
                    let (set, unset) = seat.env();
                    spec.env.extend(set);
                    spec.unset.extend(unset);
                }
                if !sandboxed {
                    return Ok((spec, seat, None));
                }
                // Its tools would run in arakun itself, outside any sandbox.
                if kind == AgentKind::WorkersAi {
                    anyhow::bail!(
                        "Workers AI runs its tools inside arakun, which has no sandbox for them yet. Turn off Sandbox agents in Settings › Project to use it here."
                    );
                }
                let sandbox = super::sandbox::Session::new(kind, &cwd)?;
                let spec = sandbox.wrap(spec);
                Ok((spec, seat, Some(Arc::new(sandbox))))
            });
            let progress = {
                let this = this.clone();
                cx.spawn(async move |cx| {
                    while let Some(line) = progress_rx.next().await {
                        let _ = this.update(cx, |t, cx| {
                            t.status_detail = Some(line.into());
                            cx.notify();
                        });
                    }
                })
            };
            let spec = resolve.await;
            progress.await;
            let _ = this.update(cx, |t, cx| match spec {
                Ok((spec, seat, sandbox)) => {
                    match seat {
                        Some(seats::Seating::On(seat)) => t.take_seat(seat, cx),
                        // Off its seat for this run; the pin stays for the next.
                        Some(seats::Seating::Off(Some(text))) => t.notice(text, false, cx),
                        _ => {}
                    }
                    t.sandbox = sandbox;
                    t.launch(spec, cx)
                }
                Err(e) => {
                    t.needs_install = e.downcast_ref::<launch::AdapterInstall>().cloned();
                    t.fail(FailedAt::Resolve, format!("{e:#}"), cx)
                }
            });
        }));
    }

    /// The session runs on `seat` from now on: saved with it (once a seat is
    /// saved, it stays) and counted as live there.
    fn take_seat(&mut self, seat: seats::Assignment, cx: &mut Context<Self>) {
        // `assign` counted it live when it picked; a pin it kept, here.
        let lease = seat.lease.unwrap_or_else(|| seats::lease(&seat.seat));
        self.lease = Some(lease);
        if self.meta.seat.as_deref() != Some(seat.seat.as_str()) || self.meta.seat_key != seat.key {
            self.meta.seat = Some(seat.seat);
            self.meta.seat_key = seat.key;
            self.meta_changed(cx);
        }
        if let Some(text) = seat.notice {
            self.notice(text, false, cx);
        }
    }

    /// The seat the agent process runs on, while it runs.
    pub fn seat(&self) -> Option<&str> {
        self.lease.as_ref().map(seats::Lease::seat)
    }

    /// Spawns on the main thread on purpose: PR_SET_PDEATHSIG fires when the
    /// *thread* that forked exits, and the main thread lives as long as arakun.
    fn launch(&mut self, spec: launch::LaunchSpec, cx: &mut Context<Self>) {
        let dialect = match self.backend {
            Backend::Acp => Dialect::Acp,
            Backend::Native => Dialect::Factory,
        };
        let spawned = if self.kind == AgentKind::WorkersAi {
            Connection::in_process(self.kind, super::workers_ai::serve)
        } else {
            Connection::spawn(self.kind, &spec, &self.cwd, dialect)
        };
        let (conn, mut rx) = match spawned {
            Ok(c) => c,
            Err(e) => {
                self.lease = None;
                return self.fail(FailedAt::Spawn, format!("{e:#}"), cx);
            }
        };
        let conn = Rc::new(conn);
        self.conn = Some(conn.clone());
        self.status_detail = Some(format!("Connecting to {}…", self.kind.label()).into());
        cx.notify();
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                if this.update(cx, |t, cx| t.incoming(msg, cx)).is_err() {
                    break;
                }
            }
        }));
        if self.backend == Backend::Native {
            return self.native_setup(conn, cx);
        }
        let cwd = self.cwd.display().to_string();
        let resume = self.session_id.clone();
        let have_history = self.suppress_replay;
        // Droid gets the arakun hint on its command line, OpenCode and Codex in
        // their environment (`launch::resolve`); Claude's adapter takes it here.
        let meta = match self.kind {
            AgentKind::Claude => json!({"systemPrompt": {"append": claude_system_hint(&self.cwd)}}),
            _ => json!({}),
        };
        self._setup = Some(cx.spawn(async move |this, cx| {
            let init = conn
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": 1,
                        "clientCapabilities": {
                            "fs": {"readTextFile": true, "writeTextFile": true},
                            "terminal": false,
                            // Claude's adapter then asks AskUserQuestion as a form.
                            "elicitation": {"form": {}},
                            // Claude's adapter then reports compactions as
                            // `compaction_update` instead of a tool call.
                            "session": {"compaction": {}},
                            // Claude's adapter then runs each subagent as a child
                            // session (AIR's form, given the extension below) and
                            // answers the prompt the user may go on past.
                            "subagents": {},
                            // Claude's adapter then sends command output as data
                            // instead of a fenced block in the tool's content.
                            "_meta": {
                                "terminal_output": true,
                                // Claude's adapter reports API retries and failures
                                // only to clients that take this extension.
                                "jetbrains": {"air": {"version": 1, "capabilities": ["sessionFailure", "sessionIndex"]}}
                            }
                        },
                        "clientInfo": {"name": "arakun", "title": "arakun", "version": env!("ARAKUN_VERSION")}
                    }),
                )
                .await;
            let init = match init {
                Ok(v) => v,
                Err(e) => {
                    let _ = this.update(cx, |t, cx| {
                        let msg = format!("initialize failed: {e}");
                        t.fail(FailedAt::Initialize(e.code), msg, cx)
                    });
                    return;
                }
            };
            let caps = &init["agentCapabilities"];
            let load = caps["loadSession"].as_bool().unwrap_or(false);
            let resume_cap = caps["sessionCapabilities"].get("resume").is_some();
            let image = caps["promptCapabilities"]["image"].as_bool().unwrap_or(false);
            let fork = caps["sessionCapabilities"].get("fork").is_some();
            let rename = [&init["_meta"], &caps["_meta"]].iter().any(|m| {
                m["jetbrains"]["air"]["capabilities"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|c| c.as_str() == Some("sessionRename")))
            });
            let _ = this.update(cx, |t, _| {
                t.caps = Caps {
                    load,
                    resume: resume_cap,
                    image,
                    fork,
                    rename,
                };
                let info = &init["agentInfo"];
                t.agent_title = str_of(info, "title")
                    .or_else(|| str_of(info, "name"))
                    .map(|n| match str_of(info, "version") {
                        Some(v) => format!("{n} {v}"),
                        None => n,
                    });
            });

            let mut session = None;
            if let Some(id) = resume {
                let params = json!({"sessionId": id, "cwd": cwd, "mcpServers": [], "_meta": meta});
                // With a transcript on screen a plain resume is enough; replaying
                // the whole history over ACP would only be thrown away.
                if have_history && resume_cap {
                    match conn.request("session/resume", params.clone()).await {
                        Ok(v) => session = Some((id.clone(), v)),
                        Err(e) => log_note(&format!("session/resume failed: {e}")),
                    }
                }
                if session.is_none() && load {
                    let _ = this.update(cx, |t, cx| {
                        t.replaying = true;
                        t.status_detail = Some("Loading the session…".into());
                        cx.notify();
                    });
                    let r = conn.request("session/load", params).await;
                    let _ = this.update(cx, |t, _| t.replaying = false);
                    match r {
                        Ok(v) => session = Some((id.clone(), v)),
                        Err(e) => log_note(&format!("session/load failed: {e}")),
                    }
                }
                if session.is_none() {
                    let _ = this.update(cx, |t, cx| {
                        t.notice(
                            "The agent could not reopen this session, so it starts fresh with no memory of the messages above.",
                            false,
                            cx,
                        )
                    });
                }
            }
            let fresh = session.is_none();
            if fresh {
                match conn
                    .request("session/new", json!({"cwd": cwd, "mcpServers": [], "_meta": meta}))
                    .await
                {
                    Ok(v) => match str_of(&v, "sessionId") {
                        Some(id) => session = Some((id, v)),
                        None => {
                            let _ = this.update(cx, |t, cx| {
                                let msg = "session/new returned no sessionId".into();
                                t.fail(FailedAt::Session(None), msg, cx)
                            });
                            return;
                        }
                    },
                    Err(e) => {
                        let _ = this.update(cx, |t, cx| {
                            let msg = t.describe_error(&e, "Starting the session failed");
                            t.fail(FailedAt::Session(Some(e.code)), msg, cx)
                        });
                        return;
                    }
                }
            }
            let Some((id, v)) = session else { return };
            let _ = this.update(cx, |t, cx| {
                // What a new session starts with is OpenCode's default; a reopened one has its own.
                if fresh && t.kind == AgentKind::OpenCode {
                    super::catalog::remember_opencode_options(&v["configOptions"]);
                }
                if fresh
                    && matches!(
                        t.kind,
                        AgentKind::Codex
                            | AgentKind::Devin
                            | AgentKind::Cursor
                            | AgentKind::Antigravity
                    )
                {
                    let raw = v["configOptions"].clone();
                    let kind = t.kind;
                    cx.background_spawn(crate::guard::task(async move {
                        match kind {
                            AgentKind::Codex => super::catalog::remember_codex_options(&raw),
                            AgentKind::Devin => super::catalog::remember_devin_options(&raw),
                            AgentKind::Cursor => super::catalog::remember_cursor_options(&raw),
                            _ => super::catalog::remember_antigravity_options(&raw),
                        }
                    }))
                    .detach();
                }
                t.session_ready(id, &v, cx)
            });
        }));
    }

    fn session_ready(&mut self, id: String, v: &Value, cx: &mut Context<Self>) {
        self.session_id = Some(id);
        self.meta.session_id = self.session_id.clone();
        self.suppress_replay = false;
        self.apply_session_state(v);
        self.status_detail = None;
        self.set_status(ThreadStatus::Idle, cx);
        self.meta_changed(cx);
        self.apply_wanted_config(cx);
        self.acp_session_ready(cx);
        self.drain_queue(cx);
    }

    fn apply_session_state(&mut self, v: &Value) {
        if let Some(m) = v.get("modes").filter(|m| m.is_object()) {
            self.modes = Some((
                str_of(m, "currentModeId").unwrap_or_default(),
                parse_choices(m.get("availableModes"), "id"),
            ));
        }
        if let Some(m) = v.get("models").filter(|m| m.is_object()) {
            self.models = Some((
                str_of(m, "currentModelId").unwrap_or_default(),
                parse_choices(m.get("availableModels"), "modelId"),
            ));
        }
        if let Some(c) = v.get("configOptions").filter(|c| c.is_array()) {
            self.config = parse_config(c);
            if self.kind == AgentKind::Claude {
                super::catalog::remember_claude_options(c);
            }
        }
    }

    fn describe_error(&mut self, e: &RpcError, what: &str) -> String {
        if e.code == RpcError::AUTH_REQUIRED || e.message.to_lowercase().contains("auth") {
            self.needs_login = true;
            let mut msg = format!("{} is not signed in.", self.kind.label());
            if let Some(hint) = &self.auth_hint {
                msg.push_str(&format!(" ({hint})"));
            }
            msg.push(' ');
            msg.push_str(self.kind.login_hint());
            msg
        } else if let Some(failure) = self.failure.take() {
            format!("{what}: {failure}")
        } else {
            format!("{what}: {e}")
        }
    }

    /// Where the session failed, while it shows the error.
    pub fn failed_at(&self) -> Option<FailedAt> {
        self.failed_at
            .filter(|_| matches!(self.status, ThreadStatus::Error(_)))
    }

    /// The failure to send to crash reports, once each. A disconnect waits: the
    /// process's exit follows it within moments and brings the exit code. When
    /// `closing`, nothing will follow, so a waiting disconnect goes out as it is.
    pub fn take_failure_report(&mut self, closing: bool) -> Option<FailedAt> {
        let at = self.failed_at()?;
        if !self.failure_unreported || (at == FailedAt::Disconnected && !closing) {
            return None;
        }
        self.failure_unreported = false;
        Some(at)
    }

    pub fn set_error_event(&mut self, event: Option<String>) {
        self.error_event = event;
    }

    pub fn error_event(&self) -> Option<&str> {
        self.error_event.as_deref()
    }

    pub fn needs_login(&self) -> bool {
        self.needs_login && matches!(self.status, ThreadStatus::Error(_))
    }

    /// The adapter the session waits for, while it waits for the user to agree.
    pub fn needs_install(&self) -> Option<&launch::AdapterInstall> {
        self.needs_install
            .as_ref()
            .filter(|_| matches!(self.status, ThreadStatus::Error(_)))
    }

    /// Spends the user's yes on one attempt: a later reconnect asks again if the
    /// adapter is gone by then.
    fn take_install_approval(&mut self) -> bool {
        std::mem::take(&mut self.install_approved)
    }

    /// The user agreed to install the adapter [`Self::needs_install`] names.
    pub fn install_adapter(&mut self, cx: &mut Context<Self>) {
        self.install_approved = true;
        self.connect(cx);
    }

    /// Asks whoever hosts the thread to run the agent's sign-in in a terminal.
    pub fn request_login(&mut self, cx: &mut Context<Self>) {
        cx.emit(ThreadEvent::LoginRequested);
    }

    /// Reconnects after an error, or re-sends the prompt that failed.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        match (
            &self.status,
            self.conn.is_some() && self.session_id.is_some(),
        ) {
            (ThreadStatus::Error(_), false) | (ThreadStatus::Connecting, false) => self.connect(cx),
            (ThreadStatus::Error(_), true) => {
                if let Some(text) = self.last_prompt.clone() {
                    self.start_turn(text, false, cx);
                } else {
                    self.set_status(ThreadStatus::Idle, cx);
                    self.drain_queue(cx);
                }
            }
            _ => {}
        }
    }

    // ---- prompting -------------------------------------------------------

    /// Whether a prompt can go out now. Settings picked before the session existed
    /// go out before the first prompt.
    fn ready(&self) -> bool {
        self.conn.is_some() && self.session_id.is_some() && !self.applying_config
    }

    /// Has the next prompt the user sends tell the agent `note`, without the
    /// chat showing it.
    pub fn add_note(&mut self, note: String) {
        // A note handed back by `take_notes` can reach the same thread twice.
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    /// The notes no prompt has taken yet. The transcript doesn't keep them, so
    /// whoever stops this thread has to.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// `text` with the notes waiting for the next prompt, each in a hint the
    /// chat doesn't show. They go with this prompt only.
    fn with_notes(&mut self, mut text: String) -> String {
        for note in std::mem::take(&mut self.notes) {
            text.push_str(&super::chart::hint(&note));
        }
        text
    }

    /// Makes this viewer the face of a session on a server.
    pub fn set_remote(&mut self) {
        self.remote = true;
    }

    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// Host-side SSH must not lend local credentials to a confined session.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn is_confined(&self) -> bool {
        self.sandbox.is_some() || std::env::var_os("ARAKUN_SANDBOX").is_some()
    }

    /// The server's queue for its session, as its `state` says it: shown as
    /// this session's own, while the server keeps and sends it.
    pub fn set_remote_queue(
        &mut self,
        queue: Vec<String>,
        paused: bool,
        sending_now: usize,
        cx: &mut Context<Self>,
    ) {
        if self.queue == queue && self.queue_paused == paused && self.urgent == sending_now {
            return;
        }
        self.urgent = sending_now.min(queue.len());
        self.queue = queue;
        self.queue_paused = paused && !self.queue.is_empty();
        self.meta_changed(cx);
    }

    pub fn fetched_images(&self) -> &HashMap<String, PathBuf> {
        &self.fetched_images
    }

    /// A copy of an image the server's session names arrived.
    pub fn image_fetched(&mut self, link: String, file: PathBuf, cx: &mut Context<Self>) {
        self.fetched_images.insert(link, file);
        cx.notify();
    }

    /// What the user did did not reach the server: the questionnaire can be
    /// answered again, and a rewind asked for is off.
    pub fn remote_undelivered(&mut self, cx: &mut Context<Self>) {
        self.native.remote_answered = None;
        self.set_remote_rewind_plan(None, None, cx);
    }

    /// What a server's session waits on, as the server words it
    /// (`phone::wire::permission` and `question`): shown with the same cards
    /// as a session here, and answered through [`ThreadEvent::Remote`].
    pub fn set_remote_asks(
        &mut self,
        permission: &Value,
        question: &Value,
        cx: &mut Context<Self>,
    ) {
        let text = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
        let permission = permission["options"]
            .as_array()
            .map(|options| PermissionRequest {
                rpc_id: Value::Null,
                tool_call_id: text(permission, "tool_call_id"),
                title: text(permission, "title"),
                options: options
                    .iter()
                    .map(|o| PermissionOption {
                        id: text(o, "id"),
                        name: text(o, "name"),
                        kind: text(o, "kind"),
                    })
                    .collect(),
                plan: None,
                session: None,
            });
        let question = native::remote_question(question);
        let asked = permission.is_some() && self.permissions.is_empty();
        self.permissions = permission.into_iter().collect();
        self.native.questions = question.into_iter().collect();
        // Another questionnaire, or none: the one answered is done with.
        let front = self
            .native
            .questions
            .front()
            .map(native::AskUserRequest::remote_key);
        if self.native.remote_answered != front {
            self.native.remote_answered = None;
        }
        if asked {
            cx.emit(ThreadEvent::NeedsPermission);
        }
        cx.emit(ThreadEvent::Changed);
        cx.notify();
    }

    /// Sends a prompt, or queues it while the agent is busy or still starting.
    pub fn send(&mut self, prompt: String, cx: &mut Context<Self>) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        if self.remote {
            cx.emit(ThreadEvent::Remote(RemoteAction::Send {
                text: prompt,
                now: false,
            }));
            return;
        }
        if !self.title_locked && self.meta.title.is_empty() {
            self.meta.title = summarize(&prompt);
            cx.emit(ThreadEvent::TitleChanged);
        }
        let ready = self.ready();
        // A turn the agent took by itself takes the prompt in (Claude's adapter queues it).
        let free = self.auto_turn && !self.cancelling && self.status == ThreadStatus::Working;
        match self.status {
            ThreadStatus::Idle | ThreadStatus::Error(_) if ready => {
                self.start_turn(prompt, true, cx)
            }
            ThreadStatus::Working if ready && free => self.start_turn(prompt, true, cx),
            _ => {
                self.queue.push(prompt);
                if matches!(self.status, ThreadStatus::Error(_)) && self.conn.is_none() {
                    self.connect(cx);
                }
                self.meta_changed(cx);
            }
        }
    }

    fn start_turn(&mut self, text: String, push_user: bool, cx: &mut Context<Self>) {
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        if let Some(plan) = self.native.rewind.take() {
            cx.emit(ThreadEvent::RewindChanged { entry: plan.entry });
        }
        // What the agent did on its own so far is logged as its own turn.
        if std::mem::take(&mut self.auto_turn) {
            let elapsed = self
                .turn_started
                .take()
                .map(|t| t.elapsed().as_millis() as u64);
            self.log_turn_usage(elapsed, cx);
        }
        let held = std::mem::take(&mut self.held);
        let message_id = (self.backend == Backend::Native).then(uuid_v4);
        let rewind_id = message_id.clone().or_else(|| self.acp_rewind_point());
        let user_ix = if push_user {
            Some(self.push_entry(
                Entry::User {
                    text: text.clone(),
                    at: unix_now(),
                    elapsed_ms: None,
                    message_id: rewind_id,
                    tokens: None,
                    shell_changes: Vec::new(),
                },
                cx,
            ))
        } else {
            if let Some(ix) = self.turn_user
                && self.backend == Backend::Native
                && let Some(Entry::User { message_id: id, .. }) = self.entries.get_mut(ix)
            {
                *id = message_id.clone();
            }
            self.turn_user
        };
        self.turn_user = user_ix;
        // A retry resends `last_prompt`, which already carries it.
        let handoff = self.handoff.as_ref().filter(|_| push_user);
        let first_here = handoff.is_some();
        // A retry resends a first prompt that already carried it.
        let carries_handoff = first_here || (!push_user && self.handoff_sent);
        let text = match handoff {
            Some(h) => {
                self.handoff_sent = true;
                format!(
                    "{h}

{text}"
                )
            }
            None => text,
        };
        // Added after the user entry, so the chat doesn't show them; `last_prompt`
        // keeps them for a retry.
        let text = if push_user {
            self.with_notes(text)
        } else {
            text
        };
        self.turn_started = Some(Instant::now());
        if push_user || self.turn_tree.is_none() {
            let cwd = self.cwd.clone();
            self.turn_tree = Some(
                cx.background_spawn(crate::guard::task(
                    async move { shell_diff::snapshot(&cwd) },
                ))
                .shared(),
            );
        }
        self.turn_tokens = self.usage.total_tokens();
        self.turn_usage = self.usage.clone();
        self.turn_tools.clear();
        self.last_prompt = Some(text.clone());
        self.last_message_id = None;
        self.cancelling = false;
        self.stuck = None;
        self.stuck_dismissed.clear();
        self.rethink = None;
        self.set_status(ThreadStatus::Working, cx);
        let images = attach::images(&text, &self.cwd);
        let first = first_here
            || self
                .entries
                .iter()
                .filter(|e| matches!(e, Entry::User { .. }))
                .count()
                <= 1;
        // Claude has it in its system prompt, so its prompts go out as typed.
        let hint = if first && self.kind != AgentKind::Claude {
            super::chart::hint(super::chart::DRAW_NOTE)
        } else {
            String::new()
        };
        if let Some(message_id) = message_id {
            return self.native_send(&conn, format!("{text}{hint}"), message_id, images, cx);
        }
        if !images.is_empty() && !self.caps.image {
            self.notice(
                format!(
                    "{} does not accept images, so only the text is sent.",
                    self.kind.label()
                ),
                false,
                cx,
            );
        }
        let images = if self.caps.image { images } else { Vec::new() };
        // The held prompt is answered once this one goes out; its response still
        // carries what it used.
        if held && let Some(old) = self._prompt.take() {
            old.detach();
        }
        self.prompt_seq += 1;
        self.prompting = true;
        let seq = self.prompt_seq;
        self._prompt = Some(cx.spawn(async move |this, cx| {
            let blocks = cx
                .background_spawn(crate::guard::task(async move {
                    images
                        .iter()
                        .map(|p| attach::acp_block(p))
                        .collect::<Vec<_>>()
                }))
                .await;
            let mut prompt = vec![json!({"type": "text", "text": text})];
            if !hint.is_empty() {
                prompt.push(json!({"type": "text", "text": hint.trim_start()}));
            }
            for block in blocks {
                match block {
                    Ok(b) => prompt.push(b),
                    Err(e) => {
                        let _ = this.update(cx, |t, cx| {
                            t.notice(format!("Not attached: {e}"), false, cx)
                        });
                    }
                }
            }
            // Stopped while the attachments were read: a `session/cancel` sent
            // before the prompt would not stop it, so it does not go out at all.
            let stopped = this
                .update(cx, |t, _| t.prompt_seq != seq || t.cancelling)
                .unwrap_or(true);
            if stopped {
                let cancelled = Ok(json!({"stopReason": "cancelled"}));
                let _ = this.update(cx, |t, cx| {
                    // The agent never got the handoff: the next prompt carries it.
                    if carries_handoff && t.prompt_seq == seq {
                        t.handoff_sent = false;
                    }
                    t.prompt_done(seq, cancelled, cx)
                });
                return;
            }
            let request = conn.request(
                "session/prompt",
                json!({"sessionId": session_id, "prompt": prompt}),
            );
            let result = request.await;
            let _ = this.update(cx, |t, cx| t.prompt_done(seq, result, cx));
        }));
    }

    /// An ACP prompt's response. One that went out before the last prompt was
    /// held for background subagents: the turn it belongs to is long settled.
    fn prompt_done(&mut self, seq: u64, result: Result<Value, RpcError>, cx: &mut Context<Self>) {
        if seq != self.prompt_seq {
            if let Ok(v) = &result {
                self.add_turn_tokens(v);
                self.meta_changed(cx);
            }
            return;
        }
        self.prompting = false;
        if !std::mem::take(&mut self.held) {
            if self.auto_turn {
                self.auto_turn = false;
                self.turn_started = self.turn_started.or(Some(Instant::now()));
            }
            return self.finish_turn(result, cx);
        }
        // The background work the response waited for is done; the turn was
        // the user's again since the agent's own part ended.
        let cancelled = std::mem::take(&mut self.cancelling);
        match &result {
            Ok(v) => {
                self.add_turn_tokens(v);
                if cancelled || v["stopReason"].as_str() == Some("cancelled") {
                    self.notice("Stopped", false, cx);
                }
            }
            Err(e) if e.code != RpcError::DISCONNECTED => {
                let msg = self.describe_error(e, "The turn failed");
                self.notice(msg, true, cx);
            }
            Err(_) => {}
        }
        if self.auto_turn {
            self.end_auto_turn(cx);
        } else {
            self.log_turn_usage(None, cx);
            if self.status == ThreadStatus::Working {
                let status = self.resting_status();
                self.set_status(status, cx);
            }
            self.meta_changed(cx);
            cx.emit(ThreadEvent::SubagentsChanged { spawned: None });
            // A prompt sent now while the subagents stopped waits for this.
            self.drain_queue(cx);
        }
    }

    /// The prompt's own work ended (Claude's adapter reports the main loop's
    /// result) while subagents it started run on: the adapter holds the
    /// response until they finish, but the user need not wait for it.
    fn release_turn(&mut self, cx: &mut Context<Self>) {
        self.held = true;
        self.stamp_turn(cx);
        let status = self.resting_status();
        self.set_status(status, cx);
        cx.emit(ThreadEvent::Finished {
            stop_reason: "end_turn".into(),
        });
        self.drain_queue(cx);
    }

    /// Output came with no turn running: the agent took one by itself.
    fn begin_auto_turn(&mut self, cx: &mut Context<Self>) {
        self.auto_turn = true;
        self.turn_started = Some(Instant::now());
        self.turn_usage = self.usage.clone();
        self.turn_tools.clear();
        self.set_status(ThreadStatus::Working, cx);
    }

    fn end_auto_turn(&mut self, cx: &mut Context<Self>) {
        self.auto_turn = false;
        self.set_retry(None, cx);
        let elapsed = self
            .turn_started
            .take()
            .map(|t| t.elapsed().as_millis() as u64);
        self.log_turn_usage(elapsed, cx);
        let status = self.resting_status();
        self.set_status(status, cx);
        self.meta_changed(cx);
        cx.emit(ThreadEvent::Finished {
            stop_reason: "end_turn".into(),
        });
        self.drain_queue(cx);
    }

    /// What the session is doing once nothing waits for the user.
    pub(super) fn resting_status(&self) -> ThreadStatus {
        let asking = !self.permissions.is_empty() || !self.native.questions.is_empty();
        let working = self.backend == Backend::Native
            || (self.prompting && !self.held)
            || self.auto_turn
            || self.cancelling
            || self.native.compacting;
        if asking {
            ThreadStatus::NeedsInput
        } else if working {
            ThreadStatus::Working
        } else {
            ThreadStatus::Idle
        }
    }

    /// What an unanswered permission request gets when the turn goes away.
    fn permission_cancelled(&self) -> Value {
        match self.backend {
            Backend::Acp => json!({"outcome": {"outcome": "cancelled"}}),
            Backend::Native => json!({"selectedOption": "cancel"}),
        }
    }

    /// Stamps the turn's duration on its prompt and drops requests nobody
    /// answered, but those of subagents still running.
    fn settle_turn(&mut self, cx: &mut Context<Self>) {
        self.stamp_turn(cx);
        let reply = self.permission_cancelled();
        let asks = std::mem::take(&mut self.permissions);
        let running = |p: &PermissionRequest| {
            p.session
                .as_deref()
                .and_then(|s| self.subagent(s))
                .is_some_and(|s| s.state.is_running())
        };
        let (keep, gone): (Vec<_>, Vec<_>) = asks.into_iter().partition(running);
        self.permissions = keep.into();
        for req in gone {
            if let Some(conn) = &self.conn {
                conn.respond(req.rpc_id.clone(), reply.clone());
            }
            self.forget_in_child(&req, cx);
        }
        self.drop_questions(true, cx);
    }

    /// Stamps the turn's duration and usage on its prompt.
    fn stamp_turn(&mut self, cx: &mut Context<Self>) {
        self.set_retry(None, cx);
        self.stuck = None;
        let from = self.turn_user.unwrap_or(0);
        if let Some(url) = self
            .entries
            .get(from..)
            .and_then(|e| e.iter().rev().find_map(pr_in_entry))
            && self.meta.pr.as_deref() != Some(url.as_str())
        {
            self.meta.pr = Some(url);
            self.meta_changed(cx);
        }
        let elapsed = self
            .turn_started
            .take()
            .map(|t| t.elapsed().as_millis() as u64);
        self.log_turn_usage(elapsed, cx);
        let used = self.usage.total_tokens().saturating_sub(self.turn_tokens);
        if let Some(ix) = self.turn_user
            && let Some(Entry::User {
                elapsed_ms, tokens, ..
            }) = self.entries.get_mut(ix)
        {
            *elapsed_ms = elapsed;
            if used > 0 {
                *tokens = Some(used);
            }
            self.entries_changed(ix, cx);
        }
        self.diff_shell_changes(cx);
    }

    /// Once a turn that ran shell commands settles, compares the checkout with
    /// how it was when the turn started.
    fn diff_shell_changes(&mut self, cx: &mut Context<Self>) {
        let Some(start) = self.turn_tree.take() else {
            return;
        };
        let Some(ix) = self.turn_user else {
            return;
        };
        let ran_shell = self
            .entries
            .get(ix + 1..)
            .unwrap_or(&[])
            .iter()
            .any(|e| matches!(e, Entry::Tool(t) if t.kind == "execute"));
        if !ran_shell {
            return;
        }
        let cwd = self.cwd.clone();
        let task = cx.spawn(async move |this, cx| {
            let Some(before) = start.await else {
                return;
            };
            let changes = cx
                .background_spawn(crate::guard::task(async move {
                    shell_diff::snapshot(&cwd)
                        .map(|after| shell_diff::changes(&before, &after))
                        .unwrap_or_default()
                }))
                .await;
            let _ = this.update(cx, |t, cx| t.set_shell_changes(ix, changes, cx));
        });
        self.shell_pending = Some(task.shared());
    }

    fn set_shell_changes(
        &mut self,
        ix: usize,
        mut changes: Vec<ShellChange>,
        cx: &mut Context<Self>,
    ) {
        let Some(after) = self.entries.get(ix + 1..) else {
            return;
        };
        let turn = &after[..after
            .iter()
            .position(|e| matches!(e, Entry::User { .. }))
            .unwrap_or(after.len())];
        // What the edit tools changed already shows with their own diffs.
        let edited: Vec<PathBuf> = turn
            .iter()
            .filter_map(|e| match e {
                Entry::Tool(t) => Some(t.content.iter()),
                _ => None,
            })
            .flatten()
            .filter_map(|c| match c {
                ToolContent::Diff { path, .. } => Some(if path.is_relative() {
                    self.cwd.join(path)
                } else {
                    path.clone()
                }),
                _ => None,
            })
            .collect();
        changes.retain(|c| !edited.contains(&c.path));
        if changes.is_empty() {
            return;
        }
        let Some(Entry::User { shell_changes, .. }) = self.entries.get_mut(ix) else {
            return;
        };
        *shell_changes = changes;
        self.entries_changed(ix, cx);
        cx.emit(ThreadEvent::ShellChanges);
    }

    /// Resolves once the last settled turn's shell changes are in.
    pub fn shell_pending(&self) -> Option<Shared<Task<()>>> {
        self.shell_pending.clone()
    }

    /// Adds what an ACP prompt response says the turn used. Claude's adapter
    /// lists the turn by model in `_meta.quota.model_usage`, subagents and
    /// compaction included, which the flat `usage` leaves out.
    fn add_turn_tokens(&mut self, v: &Value) {
        if let Some(tools) = v["_meta"]["arakun"]["tools"].as_object() {
            for (k, u) in tools {
                if let Ok(u) = serde_json::from_value::<ledger::ToolUse>(u.clone()) {
                    self.turn_tools.entry(k.clone()).or_default().add(u);
                }
            }
        }
        let rows: Vec<&Value> = v["_meta"]["quota"]["model_usage"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| &r["token_count"])
                    .filter(|t| t.is_object())
                    .collect()
            })
            .unwrap_or_default();
        let n = |t: &Value, k: &str| t[k].as_u64().unwrap_or(0);
        if !rows.is_empty() {
            for t in rows {
                self.usage.input_tokens += n(t, "inputTokens");
                self.usage.output_tokens += n(t, "outputTokens");
                self.usage.cache_read_tokens += n(t, "cachedInputTokens");
                self.usage.cache_write_tokens += n(t, "cachedWriteTokens");
            }
            return;
        }
        let u = &v["usage"];
        if u.is_object() {
            self.usage.input_tokens += n(u, "inputTokens");
            self.usage.output_tokens += n(u, "outputTokens");
            self.usage.cache_read_tokens += n(u, "cachedReadTokens");
            self.usage.cache_write_tokens += n(u, "cachedWriteTokens");
            self.usage.thinking_tokens += n(u, "thoughtTokens");
        }
    }

    /// The session's cost from the running total Claude Code just reported.
    /// A resumed session's total goes on from the saved one (newer Claude
    /// Code), or starts over at zero, as it also does after a /clear.
    fn session_cost(&mut self, reported: f64) -> f64 {
        const EPS: f64 = 1e-9;
        match self.cost_seen {
            None => {
                let saved = self.usage.cost.as_ref().map_or(0., |c| c.0);
                self.cost_base = if reported + EPS >= saved { 0. } else { saved };
            }
            Some(prev) if reported + EPS < prev => self.cost_base += prev,
            Some(_) => {}
        }
        self.cost_seen = Some(reported);
        self.cost_base + reported
    }

    /// Appends what the settled turn used to the usage log.
    fn log_turn_usage(&mut self, elapsed_ms: Option<u64>, cx: &mut Context<Self>) {
        self.last_turn_ms = elapsed_ms;
        let (now, before) = (&self.usage, &self.turn_usage);
        let model = match self.backend {
            Backend::Native => self.native.settings.as_ref().map(|s| s.model_id.clone()),
            Backend::Acp => now.model.clone(),
        };
        let turn = ledger::Turn {
            at: unix_now(),
            agent: self.kind,
            session: self.session_id.clone().unwrap_or_default(),
            cwd: self.cwd.clone(),
            model,
            input: now.input_tokens.saturating_sub(before.input_tokens),
            output: now.output_tokens.saturating_sub(before.output_tokens),
            cache_read: now
                .cache_read_tokens
                .saturating_sub(before.cache_read_tokens),
            cache_write: now
                .cache_write_tokens
                .saturating_sub(before.cache_write_tokens),
            cost: match (&now.cost, &before.cost) {
                (Some(a), Some(b)) => (a.0 - b.0).max(0.),
                (Some(a), None) => a.0,
                _ => 0.,
            },
            cost_estimate: now.cost_estimate,
            credits: now
                .credits
                .unwrap_or(0)
                .saturating_sub(before.credits.unwrap_or(0)),
            ms: elapsed_ms.unwrap_or(0),
            tools: std::mem::take(&mut self.turn_tools),
        };
        self.turn_usage = self.usage.clone();
        if turn.is_empty() {
            return;
        }
        cx.background_spawn(crate::guard::task(async move {
            if let Err(e) = ledger::append(&turn) {
                eprintln!("arakun: could not log usage: {e}");
            }
        }))
        .detach();
    }
    fn finish_turn(&mut self, result: Result<Value, RpcError>, cx: &mut Context<Self>) {
        // Usage first, so the settled turn is stamped with what it used.
        if let Ok(v) = result.as_ref() {
            self.add_turn_tokens(v);
        }
        self.settle_turn(cx);
        self.tool_started.clear();
        let cancelled = std::mem::take(&mut self.cancelling);
        let stop_reason = match result {
            Ok(v) => {
                // Claude's adapter puts a turn's terminal failure (usage limit, API
                // error) on the prompt response, not in an update, and leaves the
                // error text out of the transcript.
                let failure = &v["_meta"]["jetbrains"]["air"]["sessionFailure"];
                if failure.is_object() {
                    self.session_failure(failure, cx);
                }
                let stop = str_of(&v, "stopReason").unwrap_or_else(|| "end_turn".into());
                match stop.as_str() {
                    "cancelled" => self.notice("Stopped", false, cx),
                    "refusal" => self.notice("The agent declined to continue.", true, cx),
                    "max_tokens" => {
                        self.notice("Stopped: the reply hit the token limit.", true, cx)
                    }
                    "max_turn_requests" => {
                        self.notice("Stopped: too many model requests in one turn.", true, cx)
                    }
                    _ => {}
                }
                self.set_status(ThreadStatus::Idle, cx);
                stop
            }
            Err(e) if cancelled && e.code != RpcError::DISCONNECTED => {
                self.notice("Stopped", false, cx);
                self.set_status(ThreadStatus::Idle, cx);
                "cancelled".into()
            }
            Err(e) => {
                let (at, msg) = if e.code == RpcError::DISCONNECTED {
                    let msg = format!("{} stopped responding.", self.kind.label());
                    (FailedAt::Disconnected, msg)
                } else {
                    (
                        FailedAt::Turn(e.code),
                        self.describe_error(&e, "The turn failed"),
                    )
                };
                self.fail(at, msg, cx);
                "error".into()
            }
        };
        if let Some(failure) = self.failure.take()
            && self.status == ThreadStatus::Idle
        {
            self.notice(failure, true, cx);
        }
        // A subagent still running may still be asking.
        if self.status == ThreadStatus::Idle && !self.permissions.is_empty() {
            self.set_status(ThreadStatus::NeedsInput, cx);
        }
        cx.emit(ThreadEvent::Finished { stop_reason });
        if self.status == ThreadStatus::Idle {
            self.drain_queue(cx);
        }
    }

    fn drain_queue(&mut self, cx: &mut Context<Self>) {
        if self.status != ThreadStatus::Idle || self.applying_config {
            return;
        }
        if let Some(prompt) = self.rethink.take() {
            return self.start_turn(prompt, true, cx);
        }
        let Some(next) = take_next(&mut self.queue, &mut self.urgent, self.queue_paused) else {
            return;
        };
        self.meta_changed(cx);
        self.start_turn(next, true, cx);
    }

    /// Stops the running turn (`session/cancel`), and the subagents running in
    /// the background with it. Queued prompts wait until resumed, those sent
    /// now too: they stay in the queue. With no turn running, the queue goes on.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.remote {
            cx.emit(ThreadEvent::Remote(RemoteAction::Cancel));
            return;
        }
        self.urgent = 0;
        let stopping = self.is_busy() || self.in_background();
        if pauses_on_cancel(stopping, self.queue.len()) {
            self.queue_paused = true;
            self.meta_changed(cx);
        }
        if !stopping {
            return;
        }
        let (Some(conn), Some(id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        match self.backend {
            Backend::Acp => conn.notify("session/cancel", json!({"sessionId": id})),
            Backend::Native => self.native_cancel(&conn, cx),
        }
        let reply = self.permission_cancelled();
        for req in std::mem::take(&mut self.permissions) {
            conn.respond(req.rpc_id.clone(), reply.clone());
            self.forget_in_child(&req, cx);
        }
        // ACP asks the client to answer what it still waits on once it cancels;
        // an AskUserQuestion form left open would hold the turn.
        self.drop_questions(false, cx);
        if self.auto_turn && !self.prompting {
            // No response ends a turn the agent took by itself.
            self.notice("Stopped", false, cx);
            self.end_auto_turn(cx);
        } else if self.prompting || (self.backend == Backend::Native && self.is_busy()) {
            // A held prompt too: it answers once the subagents stopped.
            self.cancelling = true;
            self.set_status(ThreadStatus::Working, cx);
        }
        cx.notify();
    }

    /// Interrupts the running turn and sends this queued prompt as soon as it stopped,
    /// after any sent now before it. It stays in the queue until then, so an error or a
    /// restart in between does not lose it. The rest of the queue carries on after it,
    /// unless it was paused already.
    pub fn send_now(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.queue.len() {
            return;
        }
        if ix >= self.urgent {
            let prompt = self.queue.remove(ix);
            put_next(&mut self.queue, &mut self.urgent, prompt);
            self.meta_changed(cx);
        }
        self.stop_for_urgent(cx);
    }

    /// Like `send`, but a running turn is stopped first instead of the prompt
    /// waiting in the queue behind it.
    pub fn interrupt_with(&mut self, prompt: String, cx: &mut Context<Self>) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        // The server holds the queue and the turn: it stops one for the other.
        if self.remote {
            cx.emit(ThreadEvent::Remote(RemoteAction::Send {
                text: prompt,
                now: true,
            }));
            return;
        }
        if !self.is_busy() {
            return self.send(prompt, cx);
        }
        put_next(&mut self.queue, &mut self.urgent, prompt);
        self.meta_changed(cx);
        self.stop_for_urgent(cx);
    }

    /// Stops the running turn for the prompts sent now, or sends them if none runs.
    fn stop_for_urgent(&mut self, cx: &mut Context<Self>) {
        if self.cancelling && self.is_busy() {
            // Already stopping: these go once it stopped.
        } else if self.is_busy() {
            let (paused, urgent) = (self.queue_paused, self.urgent);
            self.cancel(cx);
            // `cancel` pauses the queue and forgets what goes now, as Esc should.
            (self.queue_paused, self.urgent) = (paused, urgent);
            // A turn the agent took by itself stops at once.
            if !self.is_busy() {
                self.drain_queue(cx);
            }
        } else if matches!(self.status, ThreadStatus::Error(_)) && self.conn.is_none() {
            self.connect(cx);
        } else if matches!(self.status, ThreadStatus::Error(_)) && self.ready() {
            // After a failed turn the queue waits, but a prompt sent now goes, as `send` would.
            if let Some(next) = take_next(&mut self.queue, &mut self.urgent, true) {
                self.meta_changed(cx);
                self.start_turn(next, true, cx);
            }
        } else {
            self.drain_queue(cx);
        }
        cx.notify();
    }

    /// Sends the queued prompt `text` now, or drops it. `index` is where it was
    /// seen: on a server the queue may have moved since, so one moved is looked
    /// for by its text, and one that went already is left alone.
    pub fn unqueue(&mut self, index: usize, text: &str, now: bool, cx: &mut Context<Self>) {
        if self.remote {
            cx.emit(ThreadEvent::Remote(RemoteAction::Unqueue {
                index,
                text: text.to_string(),
                now,
            }));
            return;
        }
        let Some(ix) = queued_at(&self.queue, index, text) else {
            return;
        };
        if now {
            self.send_now(ix, cx);
        } else {
            self.remove_queued(ix, cx);
        }
    }

    pub fn remove_queued(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.queue.len() {
            self.queue.remove(ix);
            if ix < self.urgent {
                self.urgent -= 1;
            }
            if self.queue.is_empty() {
                self.queue_paused = false;
            }
            self.meta_changed(cx);
        }
    }

    /// What the running turn keeps repeating, if it seems to go in circles.
    pub fn stuck(&self) -> Option<&Stuck> {
        self.stuck
            .as_ref()
            .filter(|_| self.is_busy() && !self.cancelling)
    }

    /// Stops a turn that goes in circles and, once it stopped, asks the agent to step back.
    pub fn rethink(&mut self, cx: &mut Context<Self>) {
        let Some(stuck) = self.stuck.take() else {
            return;
        };
        self.rethink = Some(stuck.prompt());
        if self.is_busy() {
            self.cancel(cx);
        } else {
            self.drain_queue(cx);
        }
    }

    /// Leaves the turn alone and stops pointing out this pattern until the next prompt.
    pub fn keep_going(&mut self, cx: &mut Context<Self>) {
        if let Some(stuck) = self.stuck.take() {
            self.stuck_dismissed.push(stuck.key);
            cx.notify();
        }
    }

    pub fn check_offer(&self) -> Option<&str> {
        self.check_offer.as_deref()
    }

    pub fn check_offered(&self) -> bool {
        self.check_offered
    }

    /// The sandbox the agent runs in, so what runs for it can run there too.
    pub fn sandbox(&self) -> Option<Arc<super::sandbox::Session>> {
        self.sandbox.clone()
    }

    /// Offers `command` as the project's check, once per session.
    pub fn offer_check(&mut self, command: String, cx: &mut Context<Self>) {
        if self.check_offered {
            return;
        }
        self.check_offered = true;
        self.check_offer = Some(command);
        cx.emit(ThreadEvent::Changed);
        cx.notify();
    }

    pub fn answer_check_offer(&mut self, answer: CheckAnswer, cx: &mut Context<Self>) {
        if let Some(command) = self.check_offer.take() {
            cx.emit(ThreadEvent::CheckOffer { command, answer });
            cx.notify();
        }
    }

    pub fn resume_queue(&mut self, cx: &mut Context<Self>) {
        if self.remote {
            cx.emit(ThreadEvent::Remote(RemoteAction::ResumeQueue));
            return;
        }
        self.queue_paused = false;
        self.meta_changed(cx);
        self.drain_queue(cx);
    }

    // ---- permissions -----------------------------------------------------

    pub fn respond_permission(&mut self, option_id: &str, cx: &mut Context<Self>) {
        let Some(req) = self.permissions.pop_front() else {
            return;
        };
        if self.remote {
            cx.emit(ThreadEvent::Remote(RemoteAction::Permission(
                option_id.into(),
            )));
            cx.notify();
            return;
        }
        let reply = match self.backend {
            Backend::Acp => json!({"outcome": {"outcome": "selected", "optionId": option_id}}),
            Backend::Native => json!({"selectedOption": option_id}),
        };
        self.answer(req, reply, cx);
    }

    /// Sends the answer to a permission request. A subagent's thread holds a
    /// copy of what its session asks the parent; either answers it, and the
    /// other drops its copy.
    fn answer(&mut self, req: PermissionRequest, reply: Value, cx: &mut Context<Self>) {
        match self.parent.clone() {
            Some(parent) => {
                let id = req.rpc_id.clone();
                cx.defer(move |cx| {
                    let _ = parent.update(cx, |p, cx| p.answer_for_child(&id, reply, cx));
                });
            }
            None => {
                if let Some(conn) = &self.conn {
                    conn.respond(req.rpc_id.clone(), reply);
                }
                self.forget_in_child(&req, cx);
            }
        }
        self.after_input(&req.tool_call_id, cx);
    }

    /// The user answered a subagent's request in the subagent's own view.
    fn answer_for_child(&mut self, rpc_id: &Value, reply: Value, cx: &mut Context<Self>) {
        let Some(at) = self.permissions.iter().position(|p| &p.rpc_id == rpc_id) else {
            return;
        };
        let Some(req) = self.permissions.remove(at) else {
            return;
        };
        if let Some(conn) = &self.conn {
            conn.respond(req.rpc_id.clone(), reply);
        }
        self.after_input(&req.tool_call_id, cx);
    }

    /// Drops the subagent's copy of a request answered here.
    fn forget_in_child(&mut self, req: &PermissionRequest, cx: &mut Context<Self>) {
        let Some(child) = req
            .session
            .as_deref()
            .and_then(|s| self.subagent(s))
            .map(|s| s.thread.clone())
        else {
            return;
        };
        let id = req.rpc_id.clone();
        child.update(cx, |c, cx| {
            if let Some(at) = c.permissions.iter().position(|p| p.rpc_id == id)
                && let Some(req) = c.permissions.remove(at)
            {
                c.after_input(&req.tool_call_id, cx);
            }
        });
    }

    /// Picks the agent's first allow option (normally "allow once").
    pub fn allow_permission(&mut self, cx: &mut Context<Self>) {
        let id = self
            .pending_permission()
            .and_then(|p| allow_option(&p.options))
            .map(|o| o.id.clone());
        if let Some(id) = id {
            self.respond_permission(&id, cx);
        }
    }

    /// Picks the agent's first reject option, or answers "cancelled" if it offered none.
    pub fn set_auto_allow(&mut self, on: bool) {
        self.auto_allow = on;
    }

    pub fn reject_permission(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.pending_permission() else {
            return;
        };
        match p
            .options
            .iter()
            .find(|o| !o.is_allow())
            .map(|o| o.id.clone())
        {
            Some(id) => self.respond_permission(&id, cx),
            None => {
                let req = self.permissions.pop_front().unwrap();
                let reply = self.permission_cancelled();
                self.answer(req, reply, cx);
            }
        }
    }

    /// Rejects the pending call but lets the turn go on, the agent told `why`.
    /// Only arakun's own agents (Workers AI) read that; others stop as on a reject.
    pub fn decline_permission(&mut self, why: &str, cx: &mut Context<Self>) {
        let id = self
            .pending_permission()
            .and_then(|p| p.options.iter().find(|o| !o.is_allow()))
            .map(|o| o.id.clone());
        let (Some(id), Backend::Acp) = (id, self.backend) else {
            return self.reject_permission(cx);
        };
        let req = self.permissions.pop_front().unwrap();
        let outcome = json!({"outcome": "selected", "optionId": id});
        self.answer(
            req,
            json!({"outcome": outcome, "_meta": {"arakun": {"continue": true, "why": why}}}),
            cx,
        );
    }

    // ---- modes and models ------------------------------------------------

    pub fn set_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        self.set_setting(Setting::Mode, id, cx);
    }

    pub fn set_model(&mut self, id: &str, cx: &mut Context<Self>) {
        self.set_setting(Setting::Model, id, cx);
    }

    /// An ACP session's setting: a config option when the agent has one for it,
    /// else the older `session/set_mode` and `session/set_model`.
    fn acp_set_setting(&mut self, s: Setting, value: &str, cx: &mut Context<Self>) {
        if self.session_id.is_none() || self.applying_config {
            self.want_config.retain(|(w, _)| *w != s);
            self.want_config.push((s, value.to_string()));
            return;
        }
        if let Some(id) = acp_option(&self.config, s).map(|o| o.id.clone()) {
            return self.set_config(&id, value, cx);
        }
        let (slot, method, key) = match s {
            Setting::Mode => (self.modes.as_mut(), "session/set_mode", "modeId"),
            Setting::Model => (self.models.as_mut(), "session/set_model", "modelId"),
            _ => return,
        };
        if let Some((current, _)) = slot {
            *current = value.to_string();
            self.call(method, json!({ key: value }), cx);
            self.meta_changed(cx);
        }
    }

    /// Sends the settings picked before the session existed, one after another (a
    /// model change can change the efforts on offer), then lets the first prompt go.
    fn apply_wanted_config(&mut self, cx: &mut Context<Self>) {
        let wants = std::mem::take(&mut self.want_config);
        let (Some(conn), Some(session)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        if wants.is_empty() || self.backend == Backend::Native {
            return;
        }
        self.applying_config = true;
        cx.spawn(async move |this, cx| {
            for (s, value) in wants {
                let request = this.update(cx, |t, _| {
                    let (method, params) = if let Some(o) = acp_option(&t.config, s) {
                        if o.current == value || !o.options.iter().any(|c| c.id == value) {
                            return None;
                        }
                        (
                            "session/set_config_option",
                            json!({"configId": o.id, "value": value}),
                        )
                    } else {
                        let (list, method, key) = match s {
                            Setting::Mode => (&t.modes, "session/set_mode", "modeId"),
                            Setting::Model => (&t.models, "session/set_model", "modelId"),
                            _ => return None,
                        };
                        let (current, choices) = list.as_ref()?;
                        if *current == value || !choices.iter().any(|c| c.id == value) {
                            return None;
                        }
                        (method, json!({ key: value }))
                    };
                    let mut params = params;
                    params["sessionId"] = json!(session);
                    Some((method, value.clone(), conn.request(method, params)))
                });
                let Ok(Some((method, value, request))) = request else {
                    continue;
                };
                let result = request.await;
                let _ = this.update(cx, |t, cx| match result {
                    Ok(v) => {
                        t.apply_session_state(&v);
                        let slot = match s {
                            _ if acp_option(&t.config, s).is_some() => None,
                            Setting::Mode => t.modes.as_mut(),
                            Setting::Model => t.models.as_mut(),
                            _ => None,
                        };
                        if let Some((current, _)) = slot {
                            *current = value;
                        }
                        t.meta_changed(cx);
                    }
                    Err(e) => t.notice(format!("{method} failed: {e}"), true, cx),
                });
            }
            let _ = this.update(cx, |t, cx| {
                t.applying_config = false;
                t.meta_changed(cx);
                t.drain_queue(cx);
            });
        })
        .detach();
    }

    pub fn set_config(&mut self, config_id: &str, value: &str, cx: &mut Context<Self>) {
        if let Some(o) = self.config.iter_mut().find(|o| o.id == config_id) {
            o.current = value.to_string();
        }
        self.call(
            "session/set_config_option",
            json!({"configId": config_id, "value": value}),
            cx,
        );
        self.meta_changed(cx);
    }

    /// A session-scoped request whose result may carry fresh session state.
    fn call(&mut self, method: &'static str, mut params: Value, cx: &mut Context<Self>) {
        let (Some(conn), Some(id)) = (&self.conn, &self.session_id) else {
            return;
        };
        params["sessionId"] = json!(id);
        let request = conn.request(method, params);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| match result {
                Ok(v) => {
                    t.apply_session_state(&v);
                    t.meta_changed(cx);
                }
                Err(e) => t.notice(format!("{method} failed: {e}"), true, cx),
            });
        })
        .detach();
    }

    // ---- incoming --------------------------------------------------------

    fn incoming(&mut self, msg: Incoming, cx: &mut Context<Self>) {
        match msg {
            Incoming::Notification { method, params } if self.backend == Backend::Native => {
                self.native_notification(&method, &params, cx)
            }
            Incoming::Request { id, method, params } if self.backend == Backend::Native => {
                self.native_request(id, &method, params, cx)
            }
            Incoming::Notification { method, params } => match method.as_str() {
                "session/update" => match params["sessionId"].as_str() {
                    // Another session's: a subagent's, else not ours to show.
                    Some(id) if self.session_id.as_deref().is_some_and(|mine| mine != id) => {
                        self.child_update(id, &params["update"], cx)
                    }
                    _ => self.update(&params["update"], cx),
                },
                // Workers AI (arakun's own agent): a line for the transcript.
                "_arakun/notice" => {
                    let text = str_of(&params, "text").unwrap_or_default();
                    if !text.trim().is_empty() {
                        self.notice(text, params["error"].as_bool().unwrap_or(false), cx);
                    }
                }
                "_auth/status_update" => {
                    let s = &params["authStatus"];
                    self.auth_hint = (s["kind"].as_str() == Some("none"))
                        .then(|| str_of(s, "label"))
                        .flatten();
                }
                _ => {}
            },
            Incoming::Request { id, method, params } => self.request(id, &method, params, cx),
            Incoming::Closed {
                status,
                stderr_tail,
            } => {
                self.conn = None;
                self.lease = None;
                self.permissions.clear();
                self.native.questions.clear();
                self._prompt = None;
                self.turn_started = None;
                self.tool_started.clear();
                (self.prompting, self.held, self.auto_turn) = (false, false, false);
                self.stop_subagents(cx);
                let tail: Vec<&str> = stderr_tail.lines().rev().take(6).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                let mut msg = format!("{} exited ({status}).", self.kind.label());
                if !tail.is_empty() {
                    msg.push('\n');
                    msg.push_str(&tail.join("\n"));
                }
                self.fail(FailedAt::Exited(exit_code(&status)), msg, cx);
            }
        }
    }

    fn request(&mut self, id: Value, method: &str, params: Value, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        match method {
            "session/request_permission" => {
                let tool = &params["toolCall"];
                let tool_call_id = str_of(tool, "toolCallId").unwrap_or_default();
                // A subagent's call is in its own transcript; the request shows in both.
                let child = self.asking_child(&params);
                let known = |t: &Thread| {
                    t.tool_index
                        .get(&tool_call_id)
                        .and_then(|&i| match &t.entries[i] {
                            Entry::Tool(t) => Some(t.title.clone()),
                            _ => None,
                        })
                };
                let known_title = match &child {
                    Some((_, thread)) => {
                        if !tool_call_id.is_empty() && known(thread.read(cx)).is_none() {
                            thread.update(cx, |c, cx| c.upsert_tool(tool, cx));
                        }
                        known(thread.read(cx))
                    }
                    None => {
                        if !tool_call_id.is_empty() && !self.tool_index.contains_key(&tool_call_id)
                        {
                            self.upsert_tool(tool, cx);
                        }
                        known(self)
                    }
                };
                let title = str_of(tool, "title")
                    .or(known_title)
                    .unwrap_or_else(|| "Run a tool".into());
                let options: Vec<PermissionOption> = params["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| {
                        Some(PermissionOption {
                            id: str_of(o, "optionId")?,
                            name: str_of(o, "name").unwrap_or_default(),
                            kind: str_of(o, "kind").unwrap_or_default(),
                        })
                    })
                    .collect();
                // Claude's ExitPlanMode carries the plan it asks to carry out.
                let plan = str_of(&tool["rawInput"], "plan")
                    .filter(|p| !p.trim().is_empty())
                    .map(|plan| SpecPlan {
                        title: String::new(),
                        plan,
                    });
                // Nobody is there to answer: the tool call stays in the log as allowed.
                if self.auto_allow
                    && let Some(o) = allow_option(&options)
                {
                    conn.respond(
                        id,
                        json!({"outcome": {"outcome": "selected", "optionId": o.id}}),
                    );
                    if let Some(&ix) = self.tool_index.get(&tool_call_id) {
                        self.entries_changed(ix, cx);
                    }
                    return;
                }
                let req = PermissionRequest {
                    rpc_id: id,
                    tool_call_id: tool_call_id.clone(),
                    title,
                    options,
                    plan,
                    session: child.as_ref().map(|(id, _)| id.clone()),
                };
                if let Some((_, thread)) = &child {
                    let copy = req.clone();
                    thread.update(cx, |c, cx| {
                        c.permissions.push_back(copy);
                        if let Some(&ix) = c.tool_index.get(&tool_call_id) {
                            c.entries_changed(ix, cx);
                        }
                        c.set_status(ThreadStatus::NeedsInput, cx);
                        cx.emit(ThreadEvent::NeedsPermission);
                    });
                }
                self.permissions.push_back(req);
                if let Some(&ix) = self.tool_index.get(&tool_call_id) {
                    self.entries_changed(ix, cx);
                }
                self.set_status(ThreadStatus::NeedsInput, cx);
                cx.emit(ThreadEvent::NeedsPermission);
                cx.notify();
            }
            "elicitation/create" => {
                let Some(mut req) = AskUserRequest::from_form(id.clone(), &params) else {
                    conn.respond(id, json!({"action": "decline"}));
                    return;
                };
                let tool_call_id = req.tool_call_id.clone();
                // A subagent's question shows in its own transcript too.
                if let Some((session, thread)) = self.asking_child(&params) {
                    req.session = Some(session);
                    let copy = req.clone();
                    thread.update(cx, |c, cx| {
                        c.native.questions.push_back(copy);
                        if let Some(&ix) = c.tool_index.get(&tool_call_id) {
                            c.entries_changed(ix, cx);
                        }
                        c.set_status(ThreadStatus::NeedsInput, cx);
                        cx.emit(ThreadEvent::NeedsPermission);
                    });
                }
                self.native.questions.push_back(req);
                if let Some(&ix) = self.tool_index.get(&tool_call_id) {
                    self.entries_changed(ix, cx);
                }
                self.set_status(ThreadStatus::NeedsInput, cx);
                cx.emit(ThreadEvent::NeedsPermission);
                cx.notify();
            }
            // Off the main thread: a big file, or one on a slow disk, would freeze the window.
            "fs/read_text_file" => {
                let cwd = self.cwd.clone();
                let sandbox = self.sandbox.clone();
                let conn = Rc::downgrade(&conn);
                cx.spawn(async move |_, cx| {
                    let read = cx
                        .background_spawn(crate::guard::task(async move {
                            read_file(&cwd, sandbox.as_deref(), &params)
                        }))
                        .await;
                    // The agent may have gone in the meantime.
                    let Some(conn) = conn.upgrade() else {
                        return;
                    };
                    match read {
                        Ok(content) => conn.respond(id, json!({"content": content})),
                        Err(e) => conn.respond_error(id, -32002, &format!("{e:#}")),
                    }
                })
                .detach();
            }
            // Checked here, written off the main thread: a sandboxed write starts a
            // process, and a big file on a slow disk would freeze the window too.
            "fs/write_text_file" => {
                let checked = self.writable_path(&params);
                let sandbox = self.sandbox.clone();
                let content = params["content"].as_str().unwrap_or("").to_string();
                let conn = Rc::downgrade(&conn);
                cx.spawn(async move |_, cx| {
                    let written = match checked {
                        Ok(path) => {
                            cx.background_spawn(crate::guard::task(async move {
                                write_text(&path, sandbox.as_deref(), &content)
                            }))
                            .await
                        }
                        Err(e) => Err(e),
                    };
                    let Some(conn) = conn.upgrade() else {
                        return;
                    };
                    match written {
                        Ok(()) => conn.respond(id, json!({})),
                        Err(e) => conn.respond_error(id, -32002, &format!("{e:#}")),
                    }
                })
                .detach();
            }
            _ => conn.respond_error(id, -32601, &format!("method not found: {method}")),
        }
    }

    /// The subagent whose session a request came on, when it is not this one.
    fn asking_child(&self, params: &Value) -> Option<(String, Entity<Thread>)> {
        params["sessionId"]
            .as_str()
            .filter(|s| self.session_id.as_deref() != Some(*s))
            .and_then(|s| self.subagents.iter().find(|c| c.id == s))
            .map(|c| (c.id.clone(), c.thread.clone()))
    }

    /// Where the agent's `fs/write_text_file` goes, if it may write there.
    fn writable_path(&self, params: &Value) -> Result<PathBuf> {
        let path = confine(&self.cwd, params["path"].as_str().unwrap_or(""))?;
        if self.sandbox.as_ref().is_some_and(|s| !s.may_write(&path)) {
            return Err(anyhow!(
                "the sandbox does not let the agent write {}",
                path.display()
            ));
        }
        Ok(path)
    }

    // ---- session/update --------------------------------------------------

    fn update(&mut self, u: &Value, cx: &mut Context<Self>) {
        let Some(kind) = u["sessionUpdate"].as_str() else {
            return;
        };
        let history = matches!(
            kind,
            "user_message_chunk"
                | "agent_message_chunk"
                | "agent_thought_chunk"
                | "tool_call"
                | "tool_call_update"
                | "plan"
                | "subagent_spawned"
                | "subagent_state_update"
                | "subagent_update"
                | "session_message"
        );
        if history && self.replaying && self.suppress_replay {
            return;
        }
        if matches!(
            kind,
            "agent_message_chunk" | "agent_thought_chunk" | "tool_call"
        ) {
            self.set_retry(None, cx);
            // Nobody prompted this: the agent goes on by itself, as Claude does
            // once a background subagent finishes.
            if self.status == ThreadStatus::Idle
                && self.origins
                && self.parent.is_none()
                && !self.replaying
                && self.conn.is_some()
            {
                self.begin_auto_turn(cx);
            }
        }
        match kind {
            "user_message_chunk" => self.append_text(0, u, cx),
            "agent_message_chunk" => self.append_text(1, u, cx),
            "agent_thought_chunk" => self.append_text(2, u, cx),
            "tool_call" | "tool_call_update" => self.upsert_tool(u, cx),
            "plan" => {
                let entries: Vec<PlanEntry> =
                    serde_json::from_value(u["entries"].clone()).unwrap_or_default();
                self.set_plan(entries, cx);
            }
            "available_commands_update" => {
                self.commands = parse_choices(u.get("availableCommands"), "name");
                self.commands_known = true;
                cx.emit(ThreadEvent::CommandsChanged);
                self.meta_changed(cx);
            }
            "current_mode_update" => {
                let id = str_of(u, "currentModeId").unwrap_or_default();
                if let Some((current, _)) = self.modes.as_mut() {
                    *current = id.clone();
                }
                if let Some(o) = self
                    .config
                    .iter_mut()
                    .find(|o| o.category.as_deref() == Some("mode"))
                {
                    o.current = id;
                }
                self.meta_changed(cx);
            }
            "config_option_update" => {
                // An update may carry only the options that changed (Claude's
                // adapter sends just the agent while a subagent runs): the
                // others keep what they had.
                merge_config(&mut self.config, parse_config(&u["configOptions"]));
                if self.kind == AgentKind::Claude {
                    super::catalog::remember_claude_options(&u["configOptions"]);
                }
                self.meta_changed(cx);
            }
            "session_info_update" => {
                let failure = &u["_meta"]["jetbrains"]["air"]["sessionFailure"];
                if failure.is_object() {
                    if !self.replaying {
                        self.session_failure(failure, cx);
                    }
                    return;
                }
                if let Some(title) = str_of(u, "title").filter(|t| !t.trim().is_empty()) {
                    self.meta.title = title;
                    self.title_locked = true;
                    cx.emit(ThreadEvent::TitleChanged);
                    cx.notify();
                }
            }
            "compaction_update" => self.acp_compaction_update(u, cx),
            "subagent_spawned" | "subagent_state_update" | "subagent_update" => {
                self.subagent_update(u, None, cx)
            }
            "session_message" => self.session_message(u, cx),
            "usage_update" => {
                self.usage.context_used = u["used"].as_u64();
                self.usage.context_size = u["size"].as_u64();
                if let Some(c) = u.get("cost").filter(|c| c.is_object())
                    && let Some(amount) = c["amount"].as_f64().filter(|a| a.is_finite() && *a >= 0.)
                {
                    let currency = str_of(c, "currency").unwrap_or_else(|| "USD".into());
                    self.usage.cost = Some((self.session_cost(amount), currency));
                }
                if let Some(model) =
                    str_of(&u["_meta"], "_claude/model").or_else(|| str_of(&u["_meta"], "model"))
                {
                    self.usage.model = Some(model);
                }
                if let Some(limit) = u["_meta"]
                    .get("_claude/rateLimit")
                    .and_then(PlanLimit::from_json)
                {
                    self.usage.limit = Some(limit);
                }
                self.fill_compacted_after(cx);
                self.meta_changed(cx);
                // Claude's adapter reports a cost once a turn's work ends, with
                // who started it: a person, or a task notification.
                let origin = u["_meta"]["_claude/origin"]["kind"].as_str();
                self.origins |= origin.is_some();
                if u.get("cost").is_some_and(Value::is_object) && self.parent.is_none() {
                    match origin {
                        Some("human")
                            if self.prompting
                                && !self.held
                                && !self.auto_turn
                                && !self.cancelling
                                && self.running_subagents() > 0 =>
                        {
                            self.release_turn(cx)
                        }
                        Some(o) if o != "human" && self.auto_turn => self.end_auto_turn(cx),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Claude's adapter (JetBrains AIR `sessionFailure`): a warning while the
    /// turn runs is a retry in progress; anything else is an error to show.
    fn session_failure(&mut self, f: &Value, cx: &mut Context<Self>) {
        let title = str_of(f, "title").unwrap_or_default();
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        if f["severity"].as_str() == Some("warning") {
            if self.is_busy() {
                self.set_retry(Some(retry_label(title)), cx);
            } else {
                self.notice(title.to_string(), false, cx);
            }
            return;
        }
        self.set_retry(None, cx);
        let mut text = title.to_string();
        if let Some(details) = str_of(f, "details").filter(|d| !d.trim().is_empty()) {
            text.push('\n');
            text.push_str(details.trim());
        }
        if self.is_busy() {
            self.failure = Some(text);
        } else {
            self.notice(text, true, cx);
        }
    }

    /// The plan lives once per turn: a newer one replaces it in place.
    fn set_plan(&mut self, entries: Vec<PlanEntry>, cx: &mut Context<Self>) {
        let turn_start = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::User { .. }))
            .unwrap_or(0);
        let existing = self.entries[turn_start..]
            .iter()
            .position(|e| matches!(e, Entry::Plan { .. }))
            .map(|i| i + turn_start);
        match existing {
            Some(ix) => {
                self.entries[ix] = Entry::Plan { entries };
                self.entries_changed(ix, cx);
            }
            None => {
                self.push_entry(Entry::Plan { entries }, cx);
            }
        }
    }

    /// role: 0 user, 1 agent, 2 thought.
    fn append_text(&mut self, role: u8, u: &Value, cx: &mut Context<Self>) {
        let text = content_text(&u["content"]);
        if text.is_empty() {
            return;
        }
        if role == 0 && !self.title_locked && self.meta.title.is_empty() {
            self.meta.title = summarize(&text);
            cx.emit(ThreadEvent::TitleChanged);
        }
        let message_id = str_of(u, "messageId");
        let new_message = message_id.is_some()
            && self.last_message_id.is_some()
            && message_id != self.last_message_id;
        if role == 1 && message_id.is_some() {
            self.fork_point = message_id.clone();
        }
        let user_rewind = (role == 0).then(|| self.acp_rewind_point()).flatten();
        if message_id.is_some() {
            self.last_message_id = message_id;
        }
        let last = self.entries.len().wrapping_sub(1);
        let target = match (role, self.entries.last_mut()) {
            (0, Some(Entry::User { text, .. })) => Some(text),
            (1, Some(Entry::Agent { text })) if !new_message => Some(text),
            (2, Some(Entry::Thought { text })) => Some(text),
            _ => None,
        };
        match target {
            Some(existing) => {
                existing.push_str(&text);
                if role == 0 {
                    super::chart::strip_hint(existing);
                }
                self.entries_changed(last, cx);
            }
            None => {
                let entry = match role {
                    0 => Entry::User {
                        text: {
                            let mut text = text;
                            super::chart::strip_hint(&mut text);
                            text
                        },
                        at: unix_now(),
                        elapsed_ms: None,
                        message_id: user_rewind,
                        tokens: None,
                        shell_changes: Vec::new(),
                    },
                    1 => Entry::Agent { text },
                    _ => Entry::Thought { text },
                };
                self.push_entry(entry, cx);
            }
        }
    }

    fn upsert_tool(&mut self, u: &Value, cx: &mut Context<Self>) {
        let Some(id) = str_of(u, "toolCallId") else {
            return;
        };
        let ix = match self.tool_index.get(&id) {
            Some(&ix) => ix,
            None => self.push_entry(
                Entry::Tool(ToolCall {
                    id: id.clone(),
                    title: String::new(),
                    kind: "other".into(),
                    status: ToolStatus::Pending,
                    content: Vec::new(),
                    locations: Vec::new(),
                    raw_input: None,
                    raw_output: None,
                    added: 0,
                    removed: 0,
                    terminal_output: String::new(),
                    terminal_dropped_lines: 0,
                    exit_code: None,
                    parent: None,
                    tool_name: None,
                }),
                cx,
            ),
        };
        let Some(Entry::Tool(t)) = self.entries.get_mut(ix) else {
            return;
        };
        if let Some(title) = str_of(u, "title") {
            t.title = title;
        }
        let claude = &u["_meta"]["claudeCode"];
        if let Some(parent) = str_of(claude, "parentToolUseId") {
            t.parent = Some(parent);
        }
        if let Some(name) = str_of(claude, "toolName") {
            t.tool_name = Some(name);
        }
        if let Some(kind) = str_of(u, "kind") {
            t.kind = kind;
        }
        if let Some(status) = u["status"].as_str() {
            t.status = parse_tool_status(status);
        }
        let status = t.status;
        self.clock_tool(&id, status);
        let Some(Entry::Tool(t)) = self.entries.get_mut(ix) else {
            return;
        };
        if let Some(content) = u.get("content").filter(|c| c.is_array()) {
            t.content = parse_tool_content(content);
            recount(t);
        }
        if let Some(locations) = u["locations"].as_array() {
            t.locations = locations
                .iter()
                .filter_map(|l| str_of(l, "path").map(PathBuf::from))
                .collect();
        }
        if let Some(input) = u.get("rawInput").filter(|v| !v.is_null()) {
            t.raw_input = Some(input.clone());
        }
        if let Some(output) = u.get("rawOutput").filter(|v| !v.is_null()) {
            t.raw_output = Some(match output.as_str() {
                Some(s) => s.to_string(),
                None => serde_json::to_string_pretty(output).unwrap_or_default(),
            });
        }
        let meta = &u["_meta"];
        for key in ["terminal_output", "terminal_output_delta"] {
            if let Some(data) = meta[key]["data"].as_str() {
                t.push_terminal_output(data);
            }
        }
        if let Some(code) = meta["terminal_exit"]["exit_code"].as_i64() {
            t.exit_code = Some(code);
        }
        self.fill_replaced(ix);
        self.entries_changed(ix, cx);
        self.mirror_subagent(ix, cx);
    }

    /// A write that replaces a whole file comes without the text it replaces,
    /// which would count every line as new. So the file is read the first time
    /// the call shows up, before it runs, and stands in for the missing text.
    fn fill_replaced(&mut self, ix: usize) {
        let Some(Entry::Tool(t)) = self.entries.get_mut(ix) else {
            return;
        };
        let done = matches!(t.status, ToolStatus::Completed | ToolStatus::Failed);
        let abs = |p: &Path| {
            if p.is_relative() {
                self.cwd.join(p)
            } else {
                p.to_path_buf()
            }
        };
        let has_diff = t
            .content
            .iter()
            .any(|c| matches!(c, ToolContent::Diff { .. }));
        let mut wanted: Vec<PathBuf> = t
            .content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Diff {
                    path,
                    old_text: None,
                    ..
                } => Some(abs(path)),
                _ => None,
            })
            .collect();
        // ACP agents may send the diff only once the write is done.
        if !has_diff && t.kind == "edit" {
            wanted.extend(t.locations.iter().map(|p| abs(p)));
        }
        if !done {
            for path in wanted {
                self.replaced
                    .entry((t.id.clone(), path.clone()))
                    .or_insert_with(|| read_small_text(&path));
            }
        }
        let mut filled = false;
        for c in &mut t.content {
            if let ToolContent::Diff {
                path,
                old_text: old @ None,
                ..
            } = c
                && let Some(Some(before)) = self.replaced.get(&(t.id.clone(), abs(path)))
            {
                *old = Some(before.clone());
                filled = true;
            }
        }
        if filled {
            recount(t);
        }
        if done {
            let id = t.id.clone();
            self.replaced.retain(|(tool, _), _| *tool != id);
        }
    }
}

/// Resolves an agent-supplied path, refusing anything outside the session folder
/// `cwd` (including through symlinks).
fn confine(cwd: &Path, raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    let path = lexical_normalize(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    });
    let root = crate::platform::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut existing = path.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| anyhow!("bad path {raw}"))?;
    }
    let mut real = crate::platform::canonicalize(existing)?;
    // `join("")` would append a trailing slash and break reads of existing files.
    let rest = path.strip_prefix(existing).unwrap_or(Path::new(""));
    if !rest.as_os_str().is_empty() {
        real = real.join(rest);
    }
    if !real.starts_with(&root) {
        return Err(anyhow!(
            "{raw} is outside the session folder {}",
            root.display()
        ));
    }
    Ok(real)
}

/// Writes the agent's `fs/write_text_file`. Runs off the main thread.
fn write_text(path: &Path, sandbox: Option<&super::sandbox::Session>, content: &str) -> Result<()> {
    // Written from inside: checked paths can still be swapped for links.
    if let Some(s) = sandbox {
        return s.write(path, content.as_bytes());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

/// Most of a file the agent gets in one answer. A bigger file would freeze the
/// window while the reply is built, and the agent can read it in parts.
const READ_MAX: usize = 8 << 20;

/// Answers the agent's `fs/read_text_file`: the whole file, or `limit` lines
/// from `line` (1-based). Runs off the main thread.
fn read_file(
    cwd: &Path,
    sandbox: Option<&super::sandbox::Session>,
    params: &Value,
) -> Result<String> {
    let path = confine(cwd, params["path"].as_str().unwrap_or(""))?;
    if sandbox.is_some_and(|s| !s.may_read(&path)) {
        return Err(anyhow!("{} is hidden from the sandbox", path.display()));
    }
    let line = params["line"].as_u64().map(|l| l.max(1) as usize);
    let limit = params["limit"].as_u64().map(|l| l as usize);
    match sandbox {
        // Read from inside: checked paths can still be swapped for links.
        Some(s) => {
            let bytes = s.read(&path, SANDBOX_READ_MAX)?;
            let len = bytes.len() as u64;
            read_lines(std::io::Cursor::new(bytes), len, &path, line, limit)
        }
        None => read_text(&path, line, limit),
    }
}

/// Most of a file read through a sandbox, which can't stream it.
const SANDBOX_READ_MAX: usize = 8 * READ_MAX;

fn read_text(path: &Path, line: Option<usize>, limit: Option<usize>) -> Result<String> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    // Reading a FIFO or a device could wait or go on for good.
    if !meta.is_file() {
        return Err(anyhow!("{} is not a regular file", path.display()));
    }
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    read_lines(std::io::BufReader::new(file), meta.len(), path, line, limit)
}

/// `limit` lines from `line` of `r`, whose size is `len`.
fn read_lines(
    mut r: impl std::io::BufRead,
    len: u64,
    path: &Path,
    line: Option<usize>,
    limit: Option<usize>,
) -> Result<String> {
    let mb = |n: u64| n.div_ceil(1 << 20);
    let too_big = || match (line, limit) {
        (None, None) => anyhow!(
            "{} is {} MB, more than the {} MB arakun reads at once. Read it in parts with `line` and `limit`.",
            path.display(),
            mb(len),
            mb(READ_MAX as u64)
        ),
        _ => anyhow!(
            "The lines asked for from {} are more than the {} MB arakun reads at once. Ask for fewer with `limit`.",
            path.display(),
            mb(READ_MAX as u64)
        ),
    };
    if line.is_none() && limit.is_none() && len > READ_MAX as u64 {
        return Err(too_big());
    }
    let read = |e| anyhow::Error::new(e).context(format!("reading {}", path.display()));
    // Lines before `line` are skipped without keeping them.
    let mut skipped = 0;
    while skipped + 1 < line.unwrap_or(1) {
        let buf = r.fill_buf().map_err(read)?;
        if buf.is_empty() {
            break;
        }
        let n = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                skipped += 1;
                i + 1
            }
            None => buf.len(),
        };
        r.consume(n);
    }
    let mut out = Vec::new();
    let mut taken = 0;
    while limit.is_none_or(|n| taken < n) {
        let buf = r.fill_buf().map_err(read)?;
        if buf.is_empty() {
            break;
        }
        let n = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                taken += 1;
                i + 1
            }
            None => buf.len(),
        };
        // The file grew past the check above, or the lines asked for are too many.
        if out.len() + n > READ_MAX {
            return Err(too_big());
        }
        out.extend_from_slice(&buf[..n]);
        r.consume(n);
    }
    String::from_utf8(out)
        .map_err(|e| read(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

fn recount(t: &mut ToolCall) {
    let (mut added, mut removed) = (0, 0);
    for c in &t.content {
        if let ToolContent::Diff {
            old_text, new_text, ..
        } = c
        {
            let (a, r) = diff::stats(&diff::line_diff(
                old_text.as_deref().unwrap_or(""),
                new_text,
            ));
            added += a;
            removed += r;
        }
    }
    t.added = added;
    t.removed = removed;
}

/// Transcripts saved before writes carried the text they replaced count every
/// rewritten line as new. Rebuilds the missing text from the session's own
/// history (the last write or edit of that file), else from the file as git
/// had it when the turn began. New files keep counting in full.
fn fill_replaced_history(entries: &mut [Entry], cwd: &Path) {
    let missing = entries.iter().any(|e| {
        matches!(e, Entry::Tool(t) if t.content.iter().any(|c| matches!(c, ToolContent::Diff { old_text: None, .. })))
    });
    if !missing {
        return;
    }
    let git = |dir: &Path, args: &[&str]| -> Option<String> {
        let out = crate::platform::output_within(
            crate::git::git(dir).args(args),
            None,
            crate::platform::GIT_LOCAL,
        )
        .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).ok())
            .flatten()
    };
    let top = git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()));
    let mut revs: HashMap<u64, Option<String>> = HashMap::new();
    let mut known: HashMap<PathBuf, String> = HashMap::new();
    let mut turn_at = 0;
    for e in entries.iter_mut() {
        let t = match e {
            Entry::User { at, .. } => {
                turn_at = *at;
                continue;
            }
            Entry::Tool(t) if t.status == ToolStatus::Completed => t,
            _ => continue,
        };
        let mut filled = false;
        for c in &mut t.content {
            let ToolContent::Diff {
                path,
                old_text,
                new_text,
            } = c
            else {
                continue;
            };
            let abs = if path.is_relative() {
                cwd.join(&*path)
            } else {
                path.clone()
            };
            match old_text {
                Some(old) => {
                    if let Some(k) = known.get_mut(&abs)
                        && let Some(at) = k.find(old.as_str())
                    {
                        k.replace_range(at..at + old.len(), new_text);
                    }
                }
                None => {
                    let before = known.get(&abs).cloned().or_else(|| {
                        let top = top.as_ref()?;
                        // Git resolves links in the toplevel, so resolve them in the path too.
                        let rel = match abs.strip_prefix(top) {
                            Ok(rel) => rel.to_path_buf(),
                            Err(_) => crate::platform::canonicalize(&abs)
                                .ok()?
                                .strip_prefix(top)
                                .ok()?
                                .to_path_buf(),
                        };
                        let rel = rel.to_str()?.to_string();
                        let rev = revs
                            .entry(turn_at)
                            .or_insert_with(|| {
                                git(
                                    top,
                                    &["rev-list", "-1", &format!("--before=@{turn_at}"), "HEAD"],
                                )
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                            })
                            .clone()?;
                        git(top, &["show", &format!("{rev}:{rel}")])
                    });
                    if before.is_some() {
                        *old_text = before;
                        filled = true;
                    }
                    known.insert(abs, new_text.clone());
                }
            }
        }
        if filled {
            recount(t);
        }
    }
}
/// A text file's contents, unless it is missing, binary or too big to diff.
fn read_small_text(path: &Path) -> Option<String> {
    const MAX: u64 = 4 << 20;
    if std::fs::metadata(path).ok()?.len() > MAX {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// "Retrying Claude, attempt 2 of 10. No response after 30s; …" →
/// "Connection error · retrying (2/10)… No response after 30s; …".
pub(super) fn retry_label(title: &str) -> String {
    let reconnect = title.starts_with("Reconnecting");
    let counts = title.split_once("attempt ").and_then(|(_, rest)| {
        let (n, rest) = rest.split_once(" of ")?;
        let m: String = rest.chars().take_while(char::is_ascii_digit).collect();
        Some((
            n.trim().parse::<u32>().ok()?,
            m.parse::<u32>().ok()?,
            rest[m.len()..].trim_start_matches('.').trim().to_string(),
        ))
    });
    let what = if reconnect {
        "Connection lost"
    } else {
        "API error"
    };
    match counts {
        Some((n, m, tail)) if tail.is_empty() => format!("{what} · retrying ({n}/{m})…"),
        Some((n, m, tail)) => format!("{what} · retrying ({n}/{m})… {tail}"),
        None => title.to_string(),
    }
}

fn log_note(line: &str) {
    if std::env::var_os("ARAKUN_ACP_TRACE").is_some() {
        eprintln!("acp: {line}");
    }
}

/// The last pull request link in what the agent wrote or a tool printed.
fn pr_in_entry(e: &Entry) -> Option<String> {
    match e {
        Entry::Agent { text } => pr_url(text),
        Entry::Tool(t) => {
            let texts = t.content.iter().filter_map(|c| match c {
                ToolContent::Text { text } => Some(text.as_str()),
                _ => None,
            });
            [t.raw_output.as_deref(), Some(t.terminal_output.as_str())]
                .into_iter()
                .flatten()
                .chain(texts)
                .filter_map(pr_url)
                .next_back()
        }
        _ => None,
    }
}

/// The last `https://github.com/<owner>/<repo>/pull/<n>` in `text`, trimmed to that.
pub fn pr_url(text: &str) -> Option<String> {
    let mut found = None;
    for (at, _) in text.match_indices("https://github.com/") {
        let rest = &text[at..];
        let end = rest
            .find(|c: char| c.is_whitespace() || "<>()[]\"'`,".contains(c))
            .unwrap_or(rest.len());
        let parts: Vec<&str> = rest[..end]
            .trim_end_matches(['.', ':', ';'])
            .split('/')
            .collect();
        // https: / "" / github.com / owner / repo / pull / n
        if parts.len() >= 7 && parts[5] == "pull" {
            let n: String = parts[6].chars().take_while(char::is_ascii_digit).collect();
            if !n.is_empty() {
                found = Some(format!(
                    "https://github.com/{}/{}/pull/{n}",
                    parts[3], parts[4]
                ));
            }
        }
    }
    found
}

/// Queues a prompt sent now: after the others sent now, before the rest.
fn put_next(queue: &mut Vec<String>, urgent: &mut usize, prompt: String) {
    *urgent = (*urgent).min(queue.len());
    queue.insert(*urgent, prompt);
    *urgent += 1;
}

/// Whether a cancel pauses the queue: only when it stops a turn. With none
/// running, the prompts that wait are waiting for the agent to start, and an
/// Esc then (on a server's session too) must not hold them back.
fn pauses_on_cancel(stopping: bool, queued: usize) -> bool {
    stopping && queued > 0
}

/// The next queued prompt to send: one sent now goes even while the queue is paused.
fn take_next(queue: &mut Vec<String>, urgent: &mut usize, paused: bool) -> Option<String> {
    if queue.is_empty() || (paused && *urgent == 0) {
        return None;
    }
    *urgent = urgent.saturating_sub(1);
    Some(queue.remove(0))
}

/// Where `text` is in `queue`: at `index` while it is still there, else at its
/// first place. None once it went.
fn queued_at(queue: &[String], index: usize, text: &str) -> Option<usize> {
    if queue.get(index).is_some_and(|q| q == text) {
        return Some(index);
    }
    queue.iter().position(|q| q == text)
}

/// Files the finished edits in `entries` touched, and those their turns'
/// shell commands changed, absolute (relative ones are in `cwd`), in
/// first-touched order.
pub fn edited_files(entries: &[Entry], cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for e in entries {
        if let Entry::User { shell_changes, .. } = e {
            for c in shell_changes {
                if !out.contains(&c.path) {
                    out.push(c.path.clone());
                }
            }
            continue;
        }
        let Entry::Tool(t) = e else { continue };
        if t.status != ToolStatus::Completed {
            continue;
        }
        let diffs = t.content.iter().filter_map(|c| match c {
            ToolContent::Diff { path, .. } => Some(path),
            _ => None,
        });
        let moved = matches!(t.kind.as_str(), "edit" | "delete" | "move").then_some(&t.locations);
        for p in diffs.chain(moved.into_iter().flatten()) {
            let p = if p.is_relative() {
                cwd.join(p)
            } else {
                p.clone()
            };
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// What `/stage` says once it ran: the text, and whether it is an error.
pub fn staged_notice(staged: Result<Vec<String>, String>) -> (String, bool) {
    match staged {
        Ok(files) if files.is_empty() => (
            "Nothing this session changed is left to stage.".to_string(),
            false,
        ),
        Ok(files) => (
            format!(
                "Staged {} {} this session changed: {}",
                files.len(),
                if files.len() == 1 { "file" } else { "files" },
                files.join(", ")
            ),
            false,
        ),
        Err(e) => (format!("Staging failed: {e}"), true),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_cancel_pauses_the_queue_only_when_it_stops_a_turn() {
        assert!(pauses_on_cancel(true, 2));
        assert!(!pauses_on_cancel(true, 0));
        // The first prompt, waiting for the agent to start.
        assert!(!pauses_on_cancel(false, 1));
    }

    #[test]
    fn a_partial_config_update_keeps_the_other_options() {
        use super::{merge_config, parse_config};
        use serde_json::json;
        let opt = |id: &str, values: &[&str]| {
            json!({"id": id, "currentValue": values[0],
                   "options": values.iter().map(|v| json!({"value": v, "name": v})).collect::<Vec<_>>()})
        };
        let mut config = parse_config(&json!([
            opt("model", &["default", "opus"]),
            opt("agent", &["default"])
        ]));
        merge_config(
            &mut config,
            parse_config(&json!([opt("agent", &["general-purpose"])])),
        );
        assert_eq!(config.len(), 2);
        assert_eq!(config[0].options.len(), 2, "the models stay");
        assert_eq!(config[1].current, "general-purpose");
    }

    #[test]
    fn command_output_keeps_its_tail_and_counts_what_it_dropped() {
        let mut t = super::ToolCall {
            id: "t".into(),
            title: String::new(),
            kind: "execute".into(),
            status: super::ToolStatus::InProgress,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
        };
        let mut total = 0u64;
        for i in 0..100_000 {
            t.push_terminal_output(&format!("line {i}\n"));
            total += 1;
            assert!(t.terminal_output.len() <= 2 * super::TERMINAL_KEEP);
        }
        let kept = t.terminal_output.lines().count() as u64;
        assert_eq!(t.terminal_dropped_lines + kept, total);
        assert!(t.terminal_output.starts_with("line "));
        assert!(t.terminal_output.ends_with("line 99999\n"));
        // Output with no line breaks is cut anyway, on a character.
        t.push_terminal_output(&"é".repeat(3 * super::TERMINAL_KEEP));
        assert!(t.terminal_output.len() <= 2 * super::TERMINAL_KEEP);
        assert!(t.terminal_output.ends_with('é'));
        // A long line that ends the output keeps its end, not nothing.
        t.push_terminal_output(&format!("{}\n", "x".repeat(3 * super::TERMINAL_KEEP)));
        assert!(t.terminal_output.len() >= super::TERMINAL_KEEP);
        assert!(t.terminal_output.ends_with("x\n"));
    }

    #[test]
    fn a_sessions_seat_is_saved_with_it_and_kept_on_resume() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        assert_eq!(t.meta().seat, None);
        t.meta.seat = Some("work".into());
        t.meta.seat_key = Some(seats::fingerprint("tok"));
        // Saved in the session list and the transcript, and read back.
        let json = serde_json::to_string(&t.meta()).unwrap();
        assert!(!json.contains("\"tok\""), "{json}");
        let meta: SessionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta.seat.as_deref(), Some("work"));
        assert_eq!(meta.seat_key, Some(seats::fingerprint("tok")));
        // A resumed or restarted session starts from that meta, so `connect`
        // asks `seats::assign` for the same seat.
        let resumed = Thread::new(AgentKind::Claude, "/p".into(), Some(meta));
        assert_eq!(resumed.meta.seat.as_deref(), Some("work"));
        // Not live until its process runs.
        assert_eq!(resumed.seat(), None);
        // Sessions saved before seats existed have none.
        let old =
            r#"{"agent":"claude","session_id":"s","title":"t","cwd":"/p","created":1,"updated":2}"#;
        let old: SessionMeta = serde_json::from_str(old).unwrap();
        assert_eq!((old.seat.as_deref(), old.seat_key.as_deref()), (None, None));
        assert!(!serde_json::to_string(&old).unwrap().contains("seat"));
    }

    #[test]
    fn a_new_failure_forgets_the_earlier_report() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        t.record_failure(FailedAt::Disconnected);
        t.set_error_event(Some("first".into()));
        assert_eq!(t.error_event(), Some("first"));
        t.record_failure(FailedAt::Exited(Some(1)));
        assert_eq!(t.error_event(), None);
        assert_eq!(t.failed_at, Some(FailedAt::Exited(Some(1))));
    }

    #[test]
    fn a_disconnect_waits_for_the_exit_unless_closing() {
        let failed = |at| {
            let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
            t.status = ThreadStatus::Error("x".into());
            (t.failed_at, t.failure_unreported) = (Some(at), true);
            t
        };
        let mut t = failed(FailedAt::Disconnected);
        assert_eq!(t.take_failure_report(false), None);
        // The exit replaces the disconnect and is sent, once.
        t.failed_at = Some(FailedAt::Exited(Some(1)));
        assert_eq!(
            t.take_failure_report(false),
            Some(FailedAt::Exited(Some(1)))
        );
        assert_eq!(t.take_failure_report(false), None);
        assert_eq!(t.take_failure_report(true), None);

        let mut t = failed(FailedAt::Disconnected);
        assert_eq!(t.take_failure_report(true), Some(FailedAt::Disconnected));
        assert_eq!(t.take_failure_report(true), None);

        let mut t = failed(FailedAt::Turn(-32603));
        assert_eq!(t.take_failure_report(false), Some(FailedAt::Turn(-32603)));

        // A thread that is no longer in error has nothing to send.
        let mut t = failed(FailedAt::Spawn);
        t.status = ThreadStatus::Idle;
        assert_eq!(t.take_failure_report(true), None);
    }

    #[test]
    fn exit_codes_come_from_the_status() {
        use super::exit_code;
        assert_eq!(exit_code("exit status: 127"), Some(127));
        assert_eq!(exit_code("exit code: 1"), Some(1));
        assert_eq!(exit_code("exit code: 0xc0000005"), Some(0xc000_0005));
        assert_eq!(exit_code("signal: 9 (SIGKILL)"), Some(-9));
        assert_eq!(exit_code("stopped"), None);
        assert_eq!(exit_code("No such file or directory (os error 2)"), None);
    }

    use super::*;

    #[test]
    fn claude_learns_to_draw_from_its_system_prompt() {
        let hint = claude_system_hint(Path::new("/p"));
        assert!(hint.ends_with(crate::agent::chart::DRAW_NOTE));
        assert!(!hint.contains("<arakun-hint>"));
    }

    #[test]
    fn notes_go_with_the_next_prompt_only_and_stay_out_of_the_chat() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        t.add_note("(renamed from a to b)".into());
        let sent = t.with_notes("fix it".into());
        assert!(sent.starts_with("fix it"));
        assert!(sent.contains("<arakun-hint>\n(renamed from a to b)\n</arakun-hint>"));
        // What the chat shows of it, and what a replay shows.
        let mut shown = sent.clone();
        crate::agent::chart::strip_hint(&mut shown);
        assert_eq!(shown, "fix it");
        // Used up: the prompt after it goes out as typed.
        assert_eq!(t.with_notes("and test it".into()), "and test it");
    }

    #[test]
    fn a_stopped_thread_hands_back_only_the_notes_no_prompt_took() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        t.add_note("(renamed from a to b)".into());
        // Handed back and given again, it is still one note.
        t.add_note("(renamed from a to b)".into());
        assert_eq!(t.take_notes(), ["(renamed from a to b)"]);
        t.add_note("(renamed from b to c)".into());
        t.with_notes("fix it".into());
        assert!(t.take_notes().is_empty());
    }

    #[test]
    fn an_install_approval_is_used_once() {
        let mut t = Thread::new(AgentKind::Codex, "/p".into(), None);
        assert!(!t.take_install_approval());
        t.install_approved = true;
        assert!(t.take_install_approval());
        assert!(!t.take_install_approval());
    }

    #[test]
    fn claude_cost_survives_a_resume_and_a_restart() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        assert!((t.session_cost(0.10) - 0.10).abs() < 1e-9);
        assert!((t.session_cost(0.25) - 0.25).abs() < 1e-9);
        // /clear: Claude Code counts from zero again.
        assert!((t.session_cost(0.05) - 0.30).abs() < 1e-9);

        // Reopened with $1 saved; newer Claude Code goes on from it.
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        t.usage.cost = Some((1.0, "USD".into()));
        assert!((t.session_cost(1.2) - 1.2).abs() < 1e-9);
        // Older Claude Code starts over instead.
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        t.usage.cost = Some((1.0, "USD".into()));
        assert!((t.session_cost(0.2) - 1.2).abs() < 1e-9);
    }

    #[test]
    fn claude_turn_tokens_come_from_the_model_rows() {
        let mut t = Thread::new(AgentKind::Claude, "/p".into(), None);
        t.add_turn_tokens(&json!({
            "usage": {"inputTokens": 1, "outputTokens": 2, "cachedReadTokens": 3, "cachedWriteTokens": 4},
            "_meta": {"quota": {"model_usage": [
                {"model": "opus", "token_count": {"inputTokens": 10, "outputTokens": 20, "cachedInputTokens": 30, "cachedWriteTokens": 40}},
                {"model": "haiku", "token_count": {"inputTokens": 1, "outputTokens": 1, "cachedInputTokens": 0, "cachedWriteTokens": 0}}
            ]}}
        }));
        let u = &t.usage;
        assert_eq!(
            (
                u.input_tokens,
                u.output_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens
            ),
            (11, 21, 30, 40)
        );
        t.add_turn_tokens(&json!({"usage": {"inputTokens": 1, "outputTokens": 2, "cachedReadTokens": 3, "cachedWriteTokens": 4}}));
        assert_eq!(t.usage.cache_write_tokens, 44);
    }

    #[test]
    fn plan_limits_and_costs_read_plainly() {
        let l = PlanLimit::from_json(&json!({"status": "allowed_warning", "rateLimitType": "five_hour", "utilization": 0.82})).unwrap();
        assert_eq!(l.label().as_deref(), Some("5h limit 82%"));
        let l = PlanLimit::from_json(&json!({"status": "allowed"})).unwrap();
        assert_eq!(l.label(), None);
        let l = PlanLimit::from_json(&json!({"status": "rejected", "rateLimitType": "seven_day"}))
            .unwrap();
        assert_eq!(l.label().as_deref(), Some("weekly limit reached"));
        // A window or status this version does not know still reads.
        let l = PlanLimit::from_json(&json!({"status": "blocked", "rateLimitType": "monthly"}))
            .unwrap();
        assert_eq!(
            (l.window, l.status),
            (LimitWindow::Unknown, LimitStatus::Unknown)
        );
        assert_eq!(l.label(), None);
        let l =
            PlanLimit::from_json(&json!({"status": "allowed_warning", "rateLimitType": "monthly"}))
                .unwrap();
        assert_eq!(l.label().as_deref(), Some("plan limit near"));
        assert_eq!(fmt_cost(0.4249, "USD", true), "≈ $0.42");
        assert_eq!(fmt_cost(0.004, "USD", false), "<$0.01");
        assert_eq!(fmt_cost(1.2, "EUR", false), "1.20 EUR");
    }
    #[test]
    fn plan_statuses_keep_their_stored_names() {
        let entries: Vec<PlanEntry> = serde_json::from_value(json!([
            {"content": "a", "status": "pending"},
            {"content": "b", "status": "in_progress"},
            {"content": "c", "status": "completed"},
            // A status from a newer agent keeps the rest of the plan.
            {"content": "d", "status": "cancelled"},
        ]))
        .unwrap();
        let statuses: Vec<_> = entries.iter().map(|e| e.status).collect();
        assert_eq!(
            statuses,
            [
                PlanStatus::Pending,
                PlanStatus::InProgress,
                PlanStatus::Completed,
                PlanStatus::Unknown
            ]
        );
        let back = serde_json::to_value(&entries[..3]).unwrap();
        assert_eq!(back[1]["status"], "in_progress");
        assert_eq!(PlanStatus::parse("completed"), PlanStatus::Completed);
        assert_eq!(PlanStatus::parse("done"), PlanStatus::Unknown);
    }
    #[test]
    fn words_api_retries() {
        assert_eq!(
            retry_label("Retrying Claude, attempt 2 of 10."),
            "API error · retrying (2/10)…"
        );
        assert_eq!(
            retry_label(
                "Reconnecting to Claude, attempt 1 of 3. No response after 30s; waiting up to 60s."
            ),
            "Connection lost · retrying (1/3)… No response after 30s; waiting up to 60s."
        );
        assert_eq!(retry_label("Something else"), "Something else");
    }

    #[test]
    fn finds_pull_request_links() {
        assert_eq!(
            pr_url(
                "Created https://github.com/a/b/pull/12.\nsee https://github.com/a/b/pull/13/files"
            ),
            Some("https://github.com/a/b/pull/13".into())
        );
        assert_eq!(pr_url("https://github.com/a/b/issues/3"), None);
    }

    #[test]
    fn edited_files_are_absolute_and_listed_once() {
        let entries = vec![
            write("t1", Path::new("src/a.rs"), None, "x"),
            write("t2", Path::new("/p/src/a.rs"), Some("x"), "y"),
            write("t3", Path::new("/p/b.rs"), None, "z"),
            // A file only a shell command made counts too.
            Entry::User {
                text: "generate it".into(),
                at: 1,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: vec![ShellChange {
                    path: "/p/gen.rs".into(),
                    old: None,
                    new: "fn x() {}".into(),
                    added: 1,
                    removed: 0,
                }],
            },
        ];
        assert_eq!(
            edited_files(&entries, Path::new("/p")),
            [
                PathBuf::from("/p/src/a.rs"),
                PathBuf::from("/p/b.rs"),
                PathBuf::from("/p/gen.rs")
            ]
        );
    }

    #[test]
    fn stage_says_what_it_staged() {
        assert_eq!(
            staged_notice(Ok(Vec::new())),
            (
                "Nothing this session changed is left to stage.".into(),
                false
            )
        );
        assert_eq!(
            staged_notice(Ok(vec!["a.rs".into()])),
            ("Staged 1 file this session changed: a.rs".into(), false)
        );
        assert_eq!(
            staged_notice(Err("not a repository".into())),
            ("Staging failed: not a repository".into(), true)
        );
    }

    #[test]
    fn continued_history_cannot_rewind_and_says_who_goes_on() {
        let mut running = write("t1", Path::new("/p/a.rs"), None, "x");
        if let Entry::Tool(t) = &mut running {
            t.status = ToolStatus::InProgress;
        }
        let earlier = vec![
            Entry::User {
                text: "fix it".into(),
                at: 1,
                elapsed_ms: None,
                message_id: Some("m1".into()),
                tokens: None,
                shell_changes: Vec::new(),
            },
            running,
            Entry::Agent {
                text: "on it".into(),
            },
        ];
        let entries = continued_entries(earlier, AgentKind::Claude);
        assert_eq!(entries.len(), 4);
        assert!(
            matches!(&entries[0], Entry::User { text, message_id: None, .. } if text == "fix it")
        );
        assert!(matches!(&entries[1], Entry::Tool(t) if t.status == ToolStatus::Failed));
        assert!(matches!(&entries[2], Entry::Agent { text } if text == "on it"));
        assert!(
            matches!(&entries[3], Entry::Notice { text, error: false, .. } if text == "Continued with Claude Code")
        );
    }

    #[test]
    fn handoff_survives_a_reopen_and_older_transcripts_have_none() {
        let t: Transcript = serde_json::from_str(
            r#"{"version":1,"meta":{"agent":"claude","title":"","cwd":"/p","created":1,"updated":1},"entries":[],"handoff":"<handoff/>"}"#,
        )
        .unwrap();
        assert_eq!(t.handoff.as_deref(), Some("<handoff/>"));
        let old: Transcript =
            serde_json::from_str(r#"{"version":1,"meta":{"agent":"claude","title":"","cwd":"/p","created":1,"updated":1},"entries":[]}"#).unwrap();
        assert!(old.handoff.is_none());
        assert!(!serde_json::to_string(&old).unwrap().contains("handoff"));
    }

    #[test]
    fn subagents_survive_a_reopen_and_older_transcripts_have_none() {
        let t: Transcript = serde_json::from_str(
            r#"{"version":1,"meta":{"agent":"claude","title":"","cwd":"/p","created":1,"updated":1},"entries":[],
                "subagents":[{"id":"ab1","name":"Run tests","state":"running","started":5,"entries":[{"type":"agent","text":"ok"}]}]}"#,
        )
        .unwrap();
        assert_eq!(t.subagents.len(), 1);
        assert_eq!(t.subagents[0].state, SubagentState::Running);
        assert_eq!(t.subagents[0].entries.len(), 1);
        let old: Transcript =
            serde_json::from_str(r#"{"version":1,"meta":{"agent":"claude","title":"","cwd":"/p","created":1,"updated":1},"entries":[]}"#).unwrap();
        assert!(old.subagents.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("subagents"));
    }

    #[test]
    fn a_subagent_keeps_the_agent_call_that_started_it() {
        let read = |json: &str| -> SubagentRecord { serde_json::from_str(json).unwrap() };
        let r = read(
            r#"{"id":"child","name":"Explore","state":"completed","started":1,"call":"tool-1"}"#,
        );
        assert_eq!(r.call.as_deref(), Some("tool-1"));
        let back = serde_json::to_string(&r).unwrap();
        assert_eq!(read(&back).call.as_deref(), Some("tool-1"));
        // One standing alone has none, and saves none.
        let alone = read(r#"{"id":"child","name":"Explore","state":"completed","started":1}"#);
        assert!(alone.call.is_none());
        assert!(!serde_json::to_string(&alone).unwrap().contains("call"));
    }

    fn write(id: &str, path: &Path, old: Option<&str>, new: &str) -> Entry {
        let mut t = ToolCall {
            id: id.into(),
            title: String::new(),
            kind: "edit".into(),
            status: ToolStatus::Completed,
            content: vec![ToolContent::Diff {
                path: path.into(),
                old_text: old.map(Into::into),
                new_text: new.into(),
            }],
            locations: Vec::new(),
            raw_input: None,
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
        };
        recount(&mut t);
        Entry::Tool(t)
    }

    #[test]
    fn old_rewrites_count_only_what_changed() {
        let repo = std::env::temp_dir().join(format!("arakun-rewrites-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let tracked: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(repo.join("a.txt"), &tracked).unwrap();
        let ok = crate::git::unpinned(&mut std::process::Command::new("sh"))
            .arg("-c")
            .arg("git init -q && git add a.txt && git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -qm a")
            .current_dir(&repo)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let (a, b) = (repo.join("a.txt"), repo.join("new.txt"));
        let v2 = tracked.replace("line 50\n", "line 50 changed\n");
        let v3 = v2.replace("line 7\n", "line seven\n");
        let mut entries = vec![
            Entry::User {
                text: "go".into(),
                at: unix_now() + 5,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            // A rewrite of a tracked file, then an edit, then another rewrite.
            write("1", &a, None, &v2),
            write("2", &a, Some("line 1\n"), "first\n"),
            write("3", &a, None, &v3.replacen("line 1\n", "first\n", 1)),
            write("4", &b, None, "x\ny\n"),
        ];
        fill_replaced_history(&mut entries, &repo);
        let counts: Vec<(u32, u32)> = entries[1..]
            .iter()
            .map(|e| match e {
                Entry::Tool(t) => (t.added, t.removed),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(counts, vec![(1, 1), (1, 1), (1, 1), (2, 0)]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn agent_reads_take_the_lines_asked_for() {
        let dir = scratch("agent-read");
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree").unwrap();
        let read = |line, limit| read_text(&file, line, limit).unwrap();
        assert_eq!(read(None, None), "one\ntwo\nthree");
        assert_eq!(read(Some(2), None), "two\nthree");
        assert_eq!(read(Some(2), Some(1)), "two\n");
        assert_eq!(read(None, Some(2)), "one\ntwo\n");
        assert_eq!(read(Some(3), Some(5)), "three");
        assert_eq!(read(Some(9), None), "");
        assert_eq!(read(None, Some(0)), "");
        let params = json!({"path": "a.txt", "line": 0, "limit": 1});
        assert_eq!(read_file(&dir, None, &params).unwrap(), "one\n");
        let outside = json!({"path": "../elsewhere.txt"});
        assert!(read_file(&dir, None, &outside).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_reads_refuse_big_files_whole_but_not_in_parts() {
        let dir = scratch("agent-read-big");
        let file = dir.join("big.log");
        std::fs::write(&file, "first\nsecond\n").unwrap();
        // Sparse, so the test writes almost nothing.
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_len(2 * READ_MAX as u64).unwrap();
        let whole = read_text(&file, None, None).unwrap_err().to_string();
        assert!(whole.contains("`line` and `limit`"), "{whole}");
        assert_eq!(read_text(&file, Some(2), Some(1)).unwrap(), "second\n");
        // The rest is one long line, too long to send even when asked for by lines.
        let part = read_text(&file, Some(3), Some(1)).unwrap_err().to_string();
        assert!(part.contains("`limit`"), "{part}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn agent_reads_refuse_what_is_not_a_file() {
        let dir = scratch("agent-read-fifo");
        let fifo = dir.join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success();
        assert!(made);
        // Opening the FIFO would wait for a writer forever.
        let err = read_text(&fifo, None, None).unwrap_err().to_string();
        assert!(err.contains("not a regular file"), "{err}");
        assert!(read_text(&dir, None, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transcript_cut_off_mid_turn_reopens_with_its_tools_failed() {
        let dir = scratch("restore");
        let path = dir.join("s.json");
        let saved = |resume: bool| {
            let mut tool = match write("1", &dir.join("a.txt"), Some("a"), "b") {
                Entry::Tool(t) => t,
                _ => unreachable!(),
            };
            tool.status = ToolStatus::InProgress;
            let t = json!({
                "version": 1,
                "meta": {"agent": "claude", "title": "", "cwd": dir, "created": 1, "updated": 1},
                "entries": [Entry::Tool(tool)],
                "interrupted": true,
                "resume": resume,
            });
            std::fs::write(&path, t.to_string()).unwrap();
            Thread::read_restored(&path).unwrap().0
        };
        let t = saved(false);
        assert!(matches!(&t.entries[0], Entry::Tool(t) if t.status == ToolStatus::Failed));
        assert!(
            matches!(&t.entries[1], Entry::Notice { text, .. } if text.starts_with("Arakun closed"))
        );
        // `arakun restart` picks the turn up by itself, so there is nothing to say.
        let t = saved(true);
        assert_eq!(t.entries.len(), 1);
        assert!(Thread::read_restored(&dir.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn q(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prompts_sent_now_keep_their_order_and_go_first() {
        let (mut queue, mut urgent) = (q(&["later"]), 0);
        put_next(&mut queue, &mut urgent, "first".into());
        // A second interrupt before the turn stopped must not replace the first.
        put_next(&mut queue, &mut urgent, "second".into());
        assert_eq!(queue, q(&["first", "second", "later"]));
        assert_eq!(urgent, 2);
    }

    #[test]
    fn prompts_sent_now_go_through_a_paused_queue() {
        let (mut queue, mut urgent) = (q(&["now", "waiting"]), 1);
        assert_eq!(
            take_next(&mut queue, &mut urgent, true).as_deref(),
            Some("now")
        );
        assert_eq!(take_next(&mut queue, &mut urgent, true), None);
        assert_eq!(queue, q(&["waiting"]));
        assert_eq!(
            take_next(&mut queue, &mut urgent, false).as_deref(),
            Some("waiting")
        );
    }

    #[test]
    fn a_queued_prompt_is_found_after_the_queue_moved() {
        let queue = q(&["a", "b", "c"]);
        assert_eq!(queued_at(&queue, 1, "b"), Some(1));
        // The one in front went before the click got there.
        assert_eq!(queued_at(&queue[1..], 1, "b"), Some(0));
        // Gone already: nothing else is taken in its place.
        assert_eq!(queued_at(&queue, 1, "d"), None);
        assert_eq!(queued_at(&[], 0, "a"), None);
    }
}
