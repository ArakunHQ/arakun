//! Cloudflare Workers AI as an agent arakun runs itself: the tool loop lives
//! here and calls the chat completions API directly, so the model's reasoning
//! streams into the session, the reasoning effort is arakun's, and tokens and
//! cost are exact.
//!
//! It speaks ACP to its [`Thread`](super::Thread) like an agent process would,
//! only over in-memory channels (`Connection::in_process`), so the transcript,
//! permissions, settings and resume all go the way they do for Claude Code.
//! Each session's API messages live in `agents/workers-ai/<id>.json`, which
//! `session/resume` reopens after a restart.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::anthropic;
use super::knobs;
use super::launch;
use super::rpc::{RpcError, unix_now, uuid_v4};
use super::thread::{ConfigOption, Setting, parse_config};
use crate::headless::broker;
use crate::headless::sandbox::Jail;

/// Which API a model is called through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Api {
    /// Chat Completions, on Workers AI.
    Chat,
    /// Anthropic's Messages API, through [`anthropic`].
    Anthropic,
}

impl Api {
    fn name(self) -> &'static str {
        match self {
            Api::Chat => "Workers AI",
            Api::Anthropic => "Anthropic",
        }
    }
}

pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    pub efforts: &'static [&'static str],
    pub default_effort: &'static str,
    /// USD per million tokens: input, output, cached input.
    pub price: [f64; 3],
    /// USD per million tokens written to the prompt cache, where that is priced apart.
    pub cache_write: f64,
    pub context: u64,
    /// Refuses `"content": null` in an assistant message that only calls tools; it is sent "" instead.
    pub no_null_content: bool,
    pub api: Api,
}

pub const MODELS: [Model; 4] = [
    Model {
        id: "@cf/deepseek-ai/deepseek-v4.1-flash-1017",
        name: "DeepSeek V4.1 Flash",
        efforts: &["none", "low", "high", "max"],
        default_effort: "high",
        price: [0.30, 1.20, 0.006],
        cache_write: 0.30,
        context: 1_000_000,
        no_null_content: false,
        api: Api::Chat,
    },
    Model {
        id: "@cf/zai-org/glm-5.3",
        name: "GLM 5.3",
        efforts: &["low", "high", "max"],
        default_effort: "max",
        price: [1.40, 4.40, 0.26],
        cache_write: 1.40,
        context: 1_000_000,
        no_null_content: false,
        api: Api::Chat,
    },
    // Not a Workers Paid model: 300 requests a minute instead of 20, and no cached-input price.
    Model {
        id: "@cf/openai/gpt-oss-120b",
        name: "GPT-OSS 120B",
        efforts: &["low", "medium", "high"],
        default_effort: "medium",
        price: [0.35, 0.75, 0.35],
        cache_write: 0.35,
        context: 128_000,
        no_null_content: true,
        api: Api::Chat,
    },
    // Paid per use with an Anthropic API key (see `anthropic::endpoint`). On the bench's 51 hard
    // tasks high solved 43 to 47 a run, level with Claude Code; medium costs 44% less and solved
    // 39 and 44; low solved 36.
    Model {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5",
        efforts: &["low", "medium", "high", "xhigh", "max"],
        default_effort: "high",
        price: [2.00, 10.00, 0.10],
        cache_write: 2.50,
        context: 1_000_000,
        no_null_content: false,
        api: Api::Anthropic,
    },
];

/// How `arakun agent new --model` names these models: `workers-ai/@cf/…`.
pub const CLI_PREFIX: &str = "workers-ai/";

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn cli_models() -> Vec<String> {
    MODELS
        .iter()
        .map(|m| format!("{CLI_PREFIX}{}", m.id))
        .collect()
}

impl Model {
    /// `effort` when this model takes it, else its default: never a silent `low`.
    pub fn effort(&self, effort: Option<&str>) -> &'static str {
        effort
            .and_then(|e| self.efforts.iter().find(|x| **x == e))
            .copied()
            .unwrap_or(self.default_effort)
    }

    /// `input` is what went in fresh: neither read from the cache nor `written` to it.
    fn cost(&self, input: u64, cached: u64, written: u64, output: u64) -> f64 {
        (input as f64 * self.price[0]
            + output as f64 * self.price[1]
            + cached as f64 * self.price[2]
            + written as f64 * self.cache_write)
            / 1e6
    }
}

/// Model and reasoning as ACP config options, the way the composer menus read them.
fn config_json(model: &Model, effort: &str) -> Value {
    let models: Vec<Value> = MODELS
        .iter()
        .map(|m| {
            json!({
                "value": m.id,
                "name": m.name,
                "description": format!("${:.2} in · ${:.2} out per M tokens", m.price[0], m.price[1]),
            })
        })
        .collect();
    let efforts: Vec<Value> = model
        .efforts
        .iter()
        .map(|e| json!({"value": e, "name": e}))
        .collect();
    json!([
        {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": model.id, "options": models},
        {"id": "reasoning_effort", "name": "Reasoning", "category": "thought_level", "type": "select", "currentValue": effort, "options": efforts},
    ])
}

/// What the next session offers and starts with, after the settings `picked` for it.
pub fn draft_options(picked: &[(Setting, String)]) -> Vec<ConfigOption> {
    let get = |s: Setting| {
        picked
            .iter()
            .find(|(d, _)| *d == s)
            .map(|(_, v)| v.as_str())
    };
    let m = get(Setting::Model).and_then(model).unwrap_or(&MODELS[0]);
    parse_config(&config_json(m, m.effort(get(Setting::Effort))))
}

// ---- credentials -----------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct Credentials {
    pub account: String,
    pub token: String,
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| launch::home().join(fallback))
}

fn auth_path() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
        .join("opencode")
        .join("auth.json")
}

fn account_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
        .join("opencode")
        .join("cf-account-id")
}

/// `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` first; else the Workers AI
/// key `opencode auth login` keeps, with the account saved beside it or in
/// opencode's `cf-account-id`. The error says how to set it up.
pub fn credentials() -> Result<Credentials, String> {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let auth = std::fs::read(auth_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let file = std::fs::read_to_string(account_path()).ok();
    pick_credentials(env("CLOUDFLARE_API_TOKEN"), env("CLOUDFLARE_ACCOUNT_ID"), auth.as_ref(), file.as_deref()).ok_or_else(|| {
        format!(
            "Workers AI needs a Cloudflare API token with Workers AI access and the account id. Set \
             CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID, or run `opencode auth login`, pick Cloudflare \
             Workers AI and paste the token (it goes to {}); the account id can also go in {}.",
            auth_path().display(),
            account_path().display()
        )
    })
}

fn pick_credentials(
    token: Option<String>,
    account: Option<String>,
    auth: Option<&Value>,
    account_file: Option<&str>,
) -> Option<Credentials> {
    let entry = auth.and_then(Value::as_object).and_then(|o| {
        ["cloudflare-workers-ai", "workers-ai"]
            .iter()
            .filter_map(|k| o.get(*k))
            .chain(
                o.iter()
                    .filter(|(k, _)| k.contains("workers-ai"))
                    .map(|(_, v)| v),
            )
            .find(|v| v["key"].as_str().is_some_and(|k| !k.trim().is_empty()))
    });
    let trimmed = |v: &Value| {
        v.as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let token = token.or_else(|| trimmed(&entry?["key"]))?;
    let account = account
        .or_else(|| trimmed(&entry?["metadata"]["accountId"]))
        .or_else(|| {
            account_file
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })?;
    Some(Credentials { account, token })
}

/// Where the model calls go: an OpenAI-compatible chat completions URL, or Anthropic's
/// Messages URL.
#[derive(Debug, PartialEq)]
pub struct Endpoint {
    pub url: String,
    /// Sent as `Authorization: Bearer` (as `x-api-key` to Anthropic), when there is one.
    pub token: Option<String>,
    pub api: Api,
}

/// `ARAKUN_WORKERS_AI_URL` (a Worker that holds the token, as in Arakun Lab's
/// containers) with `ARAKUN_WORKERS_AI_KEY` if set; else Cloudflare's API with
/// [`credentials`].
pub fn endpoint() -> Result<Endpoint, String> {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if let Some(url) = env("ARAKUN_WORKERS_AI_URL") {
        return Ok(Endpoint {
            url,
            token: env("ARAKUN_WORKERS_AI_KEY"),
            api: Api::Chat,
        });
    }
    let c = credentials()?;
    let url = format!("{API}/accounts/{}/ai/v1/chat/completions", c.account);
    super::cli_login::check(super::AgentKind::WorkersAi, &url).map_err(|e| e.to_string())?;
    Ok(Endpoint {
        url,
        token: Some(c.token),
        api: Api::Chat,
    })
}

/// Where `model`'s calls go, with what they need to get there.
pub fn endpoint_for(model: &Model) -> Result<Endpoint, String> {
    match model.api {
        Api::Chat => endpoint(),
        Api::Anthropic => anthropic::endpoint().map(|(url, key)| Endpoint {
            url,
            token: Some(key),
            api: Api::Anthropic,
        }),
    }
}

// ---- the stream --------------------------------------------------------------

/// Splits a byte stream into SSE `data:` payloads; a line may end in a later chunk.
#[derive(Default)]
pub(crate) struct Sse {
    line: Vec<u8>,
}

impl Sse {
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &b in chunk {
            if b != b'\n' {
                self.line.push(b);
                continue;
            }
            let line = String::from_utf8_lossy(&self.line)
                .trim_end_matches('\r')
                .to_string();
            self.line.clear();
            if let Some(data) = line.strip_prefix("data:") {
                out.push(data.trim_start().to_string());
            }
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Call {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// What a stream adds that the session shows as it comes.
#[derive(Debug, PartialEq)]
pub(crate) enum Delta {
    Reasoning(String),
    Content(String),
    /// The call at this index got its name.
    Call(u64),
}

/// One model reply, put together from its stream.
#[derive(Debug, Default)]
pub(crate) struct Reply {
    reasoning: String,
    pub(crate) content: String,
    pub(crate) calls: BTreeMap<u64, Call>,
    usage: Option<Value>,
    pub(crate) finish: Option<String>,
    pub(crate) done: bool,
    /// The content blocks Anthropic sent, kept in the message for the next calls.
    native: Option<Value>,
}

pub(crate) fn api_error(v: &Value) -> Option<String> {
    let e = v["errors"]
        .get(0)
        .or_else(|| v.get("error"))
        .filter(|e| !e.is_null())?;
    Some(
        e["message"]
            .as_str()
            .or(e.as_str())
            .map_or_else(|| e.to_string(), str::to_string),
    )
}

impl Reply {
    /// Takes one `data:` payload; an error the stream carries comes back as `Err`.
    pub(crate) fn take(&mut self, data: &str) -> Result<Vec<Delta>, String> {
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        if let Some(e) = api_error(&v) {
            return Err(e);
        }
        if v["usage"].is_object() {
            self.usage = Some(v["usage"].clone());
        }
        let mut out = Vec::new();
        for choice in v["choices"].as_array().into_iter().flatten() {
            if let Some(f) = choice["finish_reason"].as_str() {
                self.finish = Some(f.to_string());
            }
            let d = &choice["delta"];
            // The API sends the reasoning twice, as `reasoning_content` and `reasoning`.
            if let Some(r) = d["reasoning_content"]
                .as_str()
                .or(d["reasoning"].as_str())
                .filter(|r| !r.is_empty())
            {
                self.reasoning.push_str(r);
                out.push(Delta::Reasoning(r.to_string()));
            }
            if let Some(c) = d["content"].as_str().filter(|c| !c.is_empty()) {
                self.content.push_str(c);
                out.push(Delta::Content(c.to_string()));
            }
            for tc in d["tool_calls"].as_array().into_iter().flatten() {
                let index = tc["index"].as_u64().unwrap_or(0);
                let c = self.calls.entry(index).or_default();
                if c.id.is_empty()
                    && let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty())
                {
                    c.id = id.to_string();
                }
                let named = !c.name.is_empty();
                c.name
                    .push_str(tc["function"]["name"].as_str().unwrap_or(""));
                c.arguments
                    .push_str(tc["function"]["arguments"].as_str().unwrap_or(""));
                if !named && !c.name.is_empty() {
                    if c.id.is_empty() {
                        c.id = format!("call_{}", uuid_v4());
                    }
                    out.push(Delta::Call(index));
                }
            }
        }
        Ok(out)
    }

    /// Calls the stream never named are dropped; the rest get an id.
    pub(crate) fn settle(&mut self) {
        self.calls.retain(|_, c| !c.name.is_empty());
        for c in self.calls.values_mut() {
            if c.id.is_empty() {
                c.id = format!("call_{}", uuid_v4());
            }
        }
    }

    /// The assistant message that goes back into the history.
    pub(crate) fn message(&self) -> Value {
        let content = if self.content.is_empty() {
            Value::Null
        } else {
            json!(self.content)
        };
        let mut m = json!({"role": "assistant", "content": content});
        if !self.reasoning.is_empty() {
            m["reasoning_content"] = json!(self.reasoning);
        }
        if !self.calls.is_empty() {
            let calls: Vec<Value> = self
                .calls
                .values()
                .map(|c| {
                    let args = if c.arguments.trim().is_empty() { "{}" } else { c.arguments.as_str() };
                    json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": args}})
                })
                .collect();
            m["tool_calls"] = json!(calls);
        }
        if let Some(blocks) = &self.native {
            m[anthropic::NATIVE] = blocks.clone();
        }
        m
    }
}

/// The most a reply may write, reasoning included (65,536 unless a knob says).
fn max_output() -> u64 {
    knobs::get().max_tokens
}

fn request_body(model: &Model, effort: &str, messages: &[Value]) -> Value {
    let filled: Vec<Value>;
    let chat = model.api == Api::Chat;
    let fill = |m: &Value| {
        (model.no_null_content && m["content"].is_null())
            || (chat && (!m[anthropic::NATIVE].is_null() || m["content"].is_array()))
    };
    let messages = if messages.iter().any(fill) {
        filled = messages
            .iter()
            .map(|m| {
                let mut m = m.clone();
                if model.no_null_content && m["content"].is_null() {
                    m["content"] = json!("");
                }
                // Blocks a Claude model left in the history mean nothing to another API, and the
                // photos it was shown are only a note to a text-only model.
                if chat && let Some(parts) = m["content"].as_array() {
                    let words: Vec<&str> = parts
                        .iter()
                        .map(|p| {
                            p["text"]
                                .as_str()
                                .unwrap_or("[a photo was attached here; this model cannot see it]")
                        })
                        .collect();
                    m["content"] = json!(words.join("\n\n"));
                }
                if chat && let Some(o) = m.as_object_mut() {
                    o.remove(anthropic::NATIVE);
                }
                m
            })
            .collect();
        &filled[..]
    } else {
        messages
    };
    let body = json!({
        "model": model.id,
        "messages": messages,
        "tools": tools(),
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_completion_tokens": max_output(),
        "reasoning_effort": effort,
    });
    match model.api {
        Api::Chat => body,
        Api::Anthropic => anthropic::request(&body),
    }
}

// ---- sessions ------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Session {
    id: String,
    cwd: PathBuf,
    model: String,
    effort: String,
    /// The API messages, system prompt first.
    messages: Vec<Value>,
    /// USD so far.
    #[serde(default)]
    cost: f64,
    /// Tokens the last request and its reply filled the context with.
    #[serde(default)]
    context: u64,
    /// Tool groups the user allowed for the rest of the session ("bash", "edit").
    #[serde(default)]
    always: Vec<String>,
    /// The reasoning effort each model call went out with, in order.
    #[serde(default)]
    efforts: Vec<String>,
}

const INTERRUPTED: &str = "(not run: the turn was cut off before this call finished)";

fn sessions_dir() -> PathBuf {
    launch::agents_dir().join("workers-ai")
}

impl Session {
    fn new(cwd: PathBuf) -> Self {
        Session {
            id: uuid_v4(),
            messages: vec![json!({"role": "system", "content": system_prompt(&cwd)})],
            cwd,
            model: MODELS[0].id.into(),
            effort: MODELS[0].default_effort.into(),
            ..Session::default()
        }
    }

    fn path(id: &str) -> Option<PathBuf> {
        (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .then(|| sessions_dir().join(format!("{id}.json")))
    }

    fn save(&self) {
        let Some(path) = Self::path(&self.id) else {
            return;
        };
        // The folder, not the session id: CodeQL counts the random id as
        // sensitive (code scanning alert 15), and the folder is enough to look.
        if let Err(e) = self.save_to(&path) {
            eprintln!(
                "arakun: could not save a Workers AI session in {}: {e}",
                sessions_dir().display()
            );
        }
    }

    /// The session holds the whole chat, tool output included, so only its
    /// owner may read it. `write_private` also makes the folder, 0700; making
    /// it first with `create_dir_all` would give it the default mode.
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        crate::durable::write_private(path, &serde_json::to_vec(self).unwrap_or_default())
    }

    fn load(id: &str) -> Option<Self> {
        let bytes = std::fs::read(Self::path(id)?).ok()?;
        let mut s: Session = serde_json::from_slice(&bytes).ok()?;
        repair(&mut s.messages);
        Some(s)
    }

    fn model(&self) -> &'static Model {
        model(&self.model).unwrap_or(&MODELS[0])
    }

    fn effort(&self) -> &'static str {
        self.model().effort(Some(&self.effort))
    }

    fn config(&self) -> Value {
        config_json(self.model(), self.effort())
    }

    /// About how many tokens the next request takes: what the last one took
    /// plus what came after it, at three characters a token.
    fn estimate(&self) -> u64 {
        let last = self
            .messages
            .iter()
            .rposition(|m| m["role"] == "assistant")
            .filter(|_| self.context > 0);
        let start = last.map_or(0, |i| i + 1);
        // A photo counts as the 1,600 tokens a large one takes, not as its base64.
        let chars = |m: &Value| match m["content"].as_array() {
            Some(parts) => parts
                .iter()
                .map(|p| {
                    if p["type"] == "image_url" {
                        4_800
                    } else {
                        p.to_string().len()
                    }
                })
                .sum(),
            None => m.to_string().len(),
        };
        let chars: usize = self.messages[start..].iter().map(chars).sum();
        self.context + chars as u64 / 3
    }
}

/// Where session `id` keeps its API messages.
pub fn history_path(id: &str) -> Option<PathBuf> {
    Session::path(id)
}

/// Model calls (replies) and tool calls in session `id`'s saved history.
pub fn history_counts(id: &str) -> Option<(usize, usize)> {
    let s = Session::load(id)?;
    let replies = s.messages.iter().filter(|m| m["role"] == "assistant");
    Some(replies.fold((0, 0), |(n, calls), m| {
        (
            n + 1,
            calls + m["tool_calls"].as_array().map_or(0, Vec::len),
        )
    }))
}

/// Model calls of session `id` that went out with each effort, as (high, low).
pub fn effort_counts(id: &str) -> Option<(usize, usize)> {
    let s = Session::load(id)?;
    let n = |e: &str| s.efforts.iter().filter(|x| *x == e).count();
    Some((n("high"), n("low")))
}

/// Every tool call gets its answer, so the API takes a history whose turn was
/// cut off mid-call (arakun closed or restarted under it).
fn repair(messages: &mut Vec<Value>) {
    let mut i = 0;
    while i < messages.len() {
        let ids: Vec<String> = match messages[i]["tool_calls"].as_array() {
            Some(calls) if messages[i]["role"] == "assistant" => calls
                .iter()
                .filter_map(|c| c["id"].as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        i += 1;
        if ids.is_empty() {
            continue;
        }
        let mut answered = HashSet::new();
        while i < messages.len() && messages[i]["role"] == "tool" {
            answered.insert(
                messages[i]["tool_call_id"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            );
            i += 1;
        }
        let missing: Vec<Value> = ids
            .into_iter()
            .filter(|id| !answered.contains(id))
            .map(|id| json!({"role": "tool", "tool_call_id": id, "content": INTERRUPTED}))
            .collect();
        let n = missing.len();
        messages.splice(i..i, missing);
        i += n;
    }
}

fn system_prompt(cwd: &Path) -> String {
    let date = crate::platform::local_time(unix_now())
        .map(|t| format!("{}-{:02}-{:02}", t.year, t.month + 1, t.day))
        .unwrap_or_default();
    let mut s = format!(
        "You are a coding agent working on the user's machine in {cwd}. Today is {date}.\n\
         - Do the task with your tools: read the code, change it, run commands. Relative paths are \
         relative to {cwd}; each bash call starts there in a fresh shell.\n\
         - Read a file before you edit it. Use read_file, grep and list to read and search files instead of \
         sed, cat, head, tail or nl in bash; read_file reads up to 2000 lines in one call. edit_file needs \
         old_string exactly as the file has it.\n\
         - Batch independent tool calls (reads, searches, or focused edits) in a single reply.\n\
         - Existing tests and public signatures are behavior to keep: do not alter an existing test's inputs \
         or expected values unless the request asks for that behavior to change.\n\
         - Verify before you claim: build, run the tests or the program, and look at the result. If you \
         could not verify something, say so.\n\
         - Keep replies short and plain; the user sees your tool calls and their output.",
        cwd = cwd.display()
    );
    if let Some(hint) = launch::agent_hint(Some(cwd)) {
        s.push_str("\n\n");
        s.push_str(&hint);
    }
    if let Some(jail) = jail() {
        s.push_str(&broker::prompt(&jail));
    }
    if let Some((name, text)) = project_instructions(cwd) {
        s.push_str(&format!("\n\n# Project instructions ({name})\n\n{text}"));
    }
    let rules = knobs::get().rules_text();
    if !rules.is_empty() {
        s.push_str("\n\n");
        s.push_str(&rules);
    }
    // Arakun Lab tries variants of the rules this way.
    if let Some(text) =
        std::env::var_os("ARAKUN_SYSTEM_APPEND").and_then(|p| std::fs::read_to_string(p).ok())
        && !text.trim().is_empty()
    {
        s.push_str("\n\n");
        s.push_str(text.trim_end());
    }
    s
}

/// Project instructions read into the system prompt, at most this much.
const INSTRUCTIONS_BYTES: usize = 20_000;

/// The cwd's AGENTS.md, or its CLAUDE.md when it has none: the rules other
/// agents read there, which the project wrote for any agent.
fn project_instructions(cwd: &Path) -> Option<(&'static str, String)> {
    ["AGENTS.md", "CLAUDE.md"].into_iter().find_map(|name| {
        let text = std::fs::read_to_string(cwd.join(name)).ok()?;
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let cut = floor_boundary(text, INSTRUCTIONS_BYTES);
        let more = if cut < text.len() {
            "\n[arakun: the rest of the file is cut off here]"
        } else {
            ""
        };
        Some((name, format!("{}{more}", &text[..cut])))
    })
}

/// The PATH arakun's children get, for the sandbox.
pub fn child_path() -> std::ffi::OsString {
    launch::child_path_env()
}

/// The headless host's confinement; the app's own sessions have none.
fn jail() -> Option<Arc<Jail>> {
    crate::headless::sandbox::current()
}

/// The sandbox the tools run in, when there is one.
fn sandbox() -> Option<Arc<Jail>> {
    jail().filter(|j| j.sandbox.is_some())
}

/// The tools, and a headless session's host-side ones.
fn tools() -> Value {
    let mut t = tool_specs();
    if let (Some(jail), Some(list)) = (jail(), t.as_array_mut()) {
        list.extend(broker::tool_specs(&jail.caps));
    }
    t
}

// ---- tools ---------------------------------------------------------------------

fn tool_specs() -> Value {
    let f = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({"type": "function", "function": {
            "name": name,
            "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required},
        }})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    json!([
        f(
            "bash",
            "Run a shell command with bash in the working directory; returns its output (stdout and stderr) and exit code. \
             For dev servers and watchers use `arakun run`, not a background `&`.",
            json!({"command": s("The command line."), "timeout_secs": n("Seconds before it is killed: default 600, at most 3600.")}),
            &["command"],
        ),
        f(
            "read_file",
            &format!(
                "Read a text file, with line numbers. Long files come in pieces: offset is the first line (1-based), limit how many lines (default {}).",
                knobs::get().read_lines
            ),
            json!({"path": s("File path."), "offset": n("First line, 1-based."), "limit": n("Number of lines.")}),
            &["path"],
        ),
        f(
            "write_file",
            "Create a file, or replace one whole. For changes to an existing file prefer edit_file.",
            json!({"path": s("File path."), "content": s("The whole new content.")}),
            &["path", "content"],
        ),
        f(
            "edit_file",
            "Replace old_string with new_string in a file. old_string must match the file exactly, whitespace included, \
             and appear once, unless replace_all is true.",
            json!({
                "path": s("File path."),
                "old_string": s("Text to replace."),
                "new_string": s("What replaces it."),
                "replace_all": {"type": "boolean", "description": "Replace every occurrence."},
            }),
            &["path", "old_string", "new_string"],
        ),
        f(
            "grep",
            "Search file contents for a regular expression; prints path:line:text.",
            json!({"pattern": s("The regular expression."), "path": s("File or folder (default the working directory)."), "glob": s("Only files matching this glob, like *.rs.")}),
            &["pattern"],
        ),
        f(
            "list",
            "List a folder (default the working directory); folders end with /.",
            json!({"path": s("Folder path.")}),
            &[],
        ),
        f(
            "web_fetch",
            "Read a web page; returns its final URL and its content as Markdown (or plain text). Use it instead of curl or `arakun fetch` in bash.",
            json!({"url": s("The http(s) URL."), "format": {"type": "string", "enum": ["markdown", "text"], "description": "Default markdown."}}),
            &["url"],
        ),
        f(
            "web_search",
            "Search the web; returns results with title, URL and snippet. Read the ones that matter with web_fetch.",
            json!({"query": s("What to search for."), "max_results": n("How many results: default 8, at most 20.")}),
            &["query"],
        ),
    ])
}

/// What the model is sent of a tool's output at most (20,000 unless a knob says).
fn tool_chars() -> usize {
    knobs::get().tool_chars
}
/// What a command's live output shows at most in the session.
const LIVE_CHARS: usize = 1 << 20;

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `text` within `max` bytes: its head and its longer tail, the middle cut.
fn clip_middle(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head = floor_boundary(text, max * 2 / 5);
    let mut tail = text.len() - max * 3 / 5;
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

fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let p = match raw.strip_prefix("~/") {
        Some(rest) => launch::home().join(rest),
        None => PathBuf::from(raw),
    };
    if p.is_absolute() { p } else { cwd.join(p) }
}

/// `old` replaced by `new` in `text`, and how many times; the error is for the model.
fn apply_edit(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old.is_empty() {
        return Err("old_string is empty; use write_file to create a file".into());
    }
    if old == new {
        return Err("old_string and new_string are the same".into());
    }
    let n = text.matches(old).count();
    match n {
        0 => Err("old_string is not in the file; read it again and copy the text exactly".into()),
        1 => Ok((text.replacen(old, new, 1), 1)),
        _ if replace_all => Ok((text.replace(old, new), n)),
        _ => Err(format!(
            "old_string appears {n} times; add context to make it unique, or set replace_all"
        )),
    }
}

/// A file's bytes, as the sandbox sees it when there is one.
fn read_bytes(cwd: &Path, path: &Path) -> Result<Vec<u8>, String> {
    match sandbox() {
        Some(j) => j.sandbox.as_ref().unwrap().read(cwd, path),
        None => std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display())),
    }
}

/// Writes `content`, inside the sandbox when there is one; whether it was there.
fn write_bytes(cwd: &Path, path: &Path, content: &[u8]) -> Result<bool, String> {
    if let Some(j) = sandbox() {
        return j.sandbox.as_ref().unwrap().write(cwd, path, content);
    }
    let existed = path.exists();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(existed)
}

fn read_file(cwd: &Path, args: &Value) -> Result<String, String> {
    const MAX_BYTES: usize = 100_000;
    let path = resolve(cwd, args["path"].as_str().ok_or("path is missing")?);
    let bytes = read_bytes(cwd, &path)?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(format!("{} is a binary file", path.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok("(empty file)".into());
    }
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let min_limit = if knobs::get().read_budget { 500 } else { 1 };
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(knobs::get().read_lines as u64)
        .max(min_limit) as usize;
    if offset > lines.len() {
        return Err(format!("{} has only {} lines", path.display(), lines.len()));
    }
    let mut out = String::new();
    let mut end = offset - 1;
    for line in lines.iter().skip(offset - 1).take(limit) {
        if out.len() > MAX_BYTES {
            break;
        }
        let line = &line[..floor_boundary(line, 2000)];
        out.push_str(&format!("{:>6}\t{line}\n", end + 1));
        end += 1;
    }
    if end < lines.len() {
        out.push_str(&format!(
            "… {} more lines; read on with offset={}\n",
            lines.len() - end,
            end + 1
        ));
    }
    Ok(out)
}

fn write_file(cwd: &Path, args: &Value) -> Result<String, String> {
    let path = resolve(cwd, args["path"].as_str().ok_or("path is missing")?);
    let content = args["content"].as_str().ok_or("content is missing")?;
    let existed = write_bytes(cwd, &path, content.as_bytes())?;
    let verb = if existed { "Replaced" } else { "Created" };
    Ok(format!(
        "{verb} {} ({} lines)",
        path.display(),
        content.lines().count()
    ))
}

fn edit_file(cwd: &Path, args: &Value) -> Result<String, String> {
    let path = resolve(cwd, args["path"].as_str().ok_or("path is missing")?);
    let mut text = String::from_utf8(read_bytes(cwd, &path)?)
        .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
    let mut total = 0usize;
    if let Some(list) = args["edits"].as_array().filter(|a| !a.is_empty()) {
        for (idx, item) in list.iter().enumerate() {
            let old = item["old_string"]
                .as_str()
                .ok_or_else(|| format!("edits[{idx}].old_string is missing"))?;
            let new = item["new_string"]
                .as_str()
                .ok_or_else(|| format!("edits[{idx}].new_string is missing"))?;
            let all = item["replace_all"].as_bool().unwrap_or(false);
            let (next, n) =
                apply_edit(&text, old, new, all).map_err(|e| format!("edits[{idx}]: {e}"))?;
            text = next;
            total += n;
        }
    } else {
        let old = args["old_string"].as_str().ok_or("old_string is missing")?;
        let new = args["new_string"].as_str().ok_or("new_string is missing")?;
        let (next, n) = apply_edit(
            &text,
            old,
            new,
            args["replace_all"].as_bool().unwrap_or(false),
        )?;
        text = next;
        total = n;
    }
    write_bytes(cwd, &path, text.as_bytes())?;
    Ok(format!(
        "Edited {} ({total} replacement{})",
        path.display(),
        if total == 1 { "" } else { "s" }
    ))
}

/// How long a search may run. Outside the sandbox it reads every file under `path`,
/// which on a slow network drive could otherwise never end.
const GREP_TIMEOUT: Duration = Duration::from_secs(120);

/// The `grep` tool: rg when installed, else grep. Stops when the turn is stopped.
fn grep(cwd: &Path, args: &Value, cancel: &AtomicBool) -> Result<String, String> {
    let pattern = args["pattern"].as_str().ok_or("pattern is missing")?;
    let path = args["path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .unwrap_or(".");
    let glob = args["glob"].as_str().filter(|g| !g.is_empty());
    let mut cmd = match launch::which("rg") {
        Some(rg) => {
            let mut c = crate::platform::command(rg);
            c.args([
                "-n",
                "--no-heading",
                "--color",
                "never",
                "--max-columns",
                "400",
            ]);
            if let Some(g) = glob {
                c.args(["-g", g]);
            }
            c
        }
        None => {
            let mut c = crate::platform::command("grep");
            c.args(["-rnI"]);
            if let Some(g) = glob {
                c.arg(format!("--include={g}"));
            }
            c
        }
    };
    cmd.args(["-e", pattern, "--", path]);
    let mut cmd = match sandbox() {
        Some(j) => {
            let mut inner = vec![cmd.get_program().to_string_lossy().into_owned()];
            inner.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
            j.sandbox.as_ref().unwrap().command(cwd, &inner)
        }
        None => cmd,
    };
    let ran = run_streaming(&mut cmd, cwd, GREP_TIMEOUT, cancel, |_| {})
        .map_err(|e| format!("cannot run the search: {e}"))?;
    if let Some(why) = ran.stopped {
        return Err(format!("the search {why}"));
    }
    // rg and grep exit 2 on an error, so output with 0 has only matches in it.
    match ran.exit {
        Some(0) => Ok(clip_middle(&ran.output, tool_chars())),
        Some(1) => Ok("No matches.".into()),
        _ => Err(clip_middle(ran.output.trim(), tool_chars())),
    }
}

fn list(cwd: &Path, args: &Value) -> Result<String, String> {
    const MAX: usize = 1000;
    let dir = resolve(
        cwd,
        args["path"]
            .as_str()
            .filter(|p| !p.is_empty())
            .unwrap_or("."),
    );
    let mut names: Vec<String> = match sandbox() {
        Some(j) => j.sandbox.as_ref().unwrap().list(cwd, &dir)?,
        None => std::fs::read_dir(&dir)
            .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect(),
    };
    names.sort();
    let more = names.len().saturating_sub(MAX);
    names.truncate(MAX);
    if names.is_empty() {
        return Ok("(empty folder)".into());
    }
    let mut out = names.join("\n");
    if more > 0 {
        out.push_str(&format!("\n… {more} more"));
    }
    Ok(out)
}

/// A command's whole output for the `bash_tail` knob: in memory until it
/// outgrows what the model is sent, then in a temp file the model can read.
struct Spool {
    mem: String,
    file: Option<std::fs::File>,
    /// Where it goes on this machine, and as the model's commands see it.
    host: PathBuf,
    shown: String,
    total: usize,
    failed: bool,
}

impl Spool {
    fn new(call_id: &str) -> Self {
        let name: String = call_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(60)
            .collect();
        let name = format!("arakun-bash-{name}.log");
        let (host, shown) = match sandbox() {
            Some(j) => (
                j.sandbox.as_ref().unwrap().tmp.join(&name),
                format!("/tmp/{name}"),
            ),
            None => {
                let p = std::env::temp_dir().join(&name);
                let shown = p.display().to_string();
                (p, shown)
            }
        };
        Spool {
            mem: String::new(),
            file: None,
            host,
            shown,
            total: 0,
            failed: false,
        }
    }

    fn push(&mut self, text: &str) {
        use std::io::Write as _;
        self.total += text.len();
        if self.failed {
            return;
        }
        if self.file.is_none() {
            self.mem.push_str(text);
            if self.mem.len() <= tool_chars() {
                return;
            }
            match std::fs::File::create(&self.host) {
                Ok(f) => self.file = Some(f),
                Err(_) => {
                    self.failed = true;
                    return;
                }
            }
        }
        let chunk = if self.mem.is_empty() {
            text.as_bytes()
        } else {
            self.mem.as_bytes()
        };
        let written = self.file.as_mut().unwrap().write_all(chunk);
        self.mem.clear();
        if written.is_err() {
            self.failed = true;
        }
    }

    /// The file's path for the model and the output's size, when it went to a file.
    fn saved(&self) -> Option<(String, usize)> {
        (self.file.is_some() && !self.failed).then(|| (self.shown.clone(), self.total))
    }
}

/// How a command ended.
struct Ran {
    output: String,
    exit: Option<i64>,
    /// Why arakun ended it, when it did.
    stopped: Option<String>,
}

/// Runs `command` in `cwd`, handing its output to `live` as it comes; ends it
/// after `timeout`, or when `cancel` is set.
fn run_bash(
    cwd: &Path,
    command: &str,
    timeout: Duration,
    cancel: &AtomicBool,
    live: impl FnMut(&str),
) -> Result<Ran, String> {
    let shell = if launch::which("bash").is_some() {
        "bash"
    } else {
        "sh"
    };
    let mut cmd = match sandbox() {
        Some(j) => j
            .sandbox
            .as_ref()
            .unwrap()
            .command(cwd, &[shell.into(), "-c".into(), command.into()]),
        None => {
            let mut c = crate::platform::agent_command(shell);
            c.arg("-c")
                .arg(command)
                .env("PATH", launch::child_path_env());
            c
        }
    };
    run_streaming(&mut cmd, cwd, timeout, cancel, live)
        .map_err(|e| format!("cannot start {shell}: {e}"))
}

/// Runs `cmd` in `cwd`, stdout and stderr together, handing its output to `live` as
/// it comes and keeping only its head and tail; ends it and what it started after
/// `timeout`, or when `cancel` is set.
fn run_streaming(
    cmd: &mut std::process::Command,
    cwd: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
    mut live: impl FnMut(&str),
) -> std::io::Result<Ran> {
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::platform::ProcessTree::prepare(cmd);
    crate::platform::lower_priority(cmd);
    let mut child = cmd.spawn()?;
    let tree = crate::platform::ProcessTree::adopt(&child);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let pipes: [Option<Box<dyn std::io::Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ];
    for mut pipe in pipes.into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let started = Instant::now();
    let (mut output, mut pending) = (String::new(), Vec::<u8>::new());
    let (mut exit, mut exited_at, mut stopped) = (None, None::<Instant>, None);
    loop {
        let closed = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(bytes) => {
                pending.extend_from_slice(&bytes);
                let valid = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => pending.len(),
                };
                let text = String::from_utf8_lossy(&pending[..valid]).to_string();
                pending.drain(..valid);
                live(&text);
                output.push_str(&text);
                // Only the head and tail go to the model; the middle needn't pile up.
                if output.len() > 8 * tool_chars() {
                    let keep = floor_boundary(&output, tool_chars());
                    let tail = output.len() - 4 * tool_chars();
                    let tail = (tail..output.len())
                        .find(|i| output.is_char_boundary(*i))
                        .unwrap_or(output.len());
                    output = format!("{}\n… [output cut] …\n{}", &output[..keep], &output[tail..]);
                }
                false
            }
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            Err(mpsc::RecvTimeoutError::Disconnected) => true,
        };
        if exited_at.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            exit = status.code().map(i64::from);
            exited_at = Some(Instant::now());
        }
        // Something it left running in the background may hold the pipes open.
        if exited_at.is_some_and(|t| closed || t.elapsed() > Duration::from_secs(1)) {
            break;
        }
        if closed && exited_at.is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
        let why = if cancel.load(Ordering::SeqCst) {
            Some("stopped".to_string())
        } else if started.elapsed() > timeout {
            Some(format!("timed out after {}s", timeout.as_secs()))
        } else {
            None
        };
        if let Some(why) = why {
            tree.kill();
            let _ = child.wait();
            stopped = Some(why);
            break;
        }
    }
    Ok(Ran {
        output,
        exit,
        stopped,
    })
}

/// Tools that only look, which may run side by side.
fn read_only(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "grep" | "list" | "web_fetch" | "web_search"
    )
}

/// Tools that change things ask first, unless the session runs on its own.
fn permission_group(name: &str) -> Option<&'static str> {
    match name {
        "bash" => Some("bash"),
        "write_file" | "edit_file" => Some("edit"),
        _ => None,
    }
}

fn tool_kind(name: &str) -> &'static str {
    match name {
        "bash" => "execute",
        "read_file" | "list" | "read_email" => "read",
        "write_file" | "edit_file" => "edit",
        "grep" => "search",
        "web_fetch" | "web_search" => "fetch",
        _ => "other",
    }
}

/// The title, locations and content a call shows with, ACP-style.
fn describe(name: &str, args: &Value, cwd: &Path) -> Value {
    let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
    let path = s("path");
    let title = match name {
        "bash" => s("command").lines().next().unwrap_or("").to_string(),
        "read_file" => format!("Read {path}"),
        "write_file" => format!("Write {path}"),
        "edit_file" => format!("Edit {path}"),
        "grep" => format!("Grep {}", s("pattern")),
        "list" => format!("List {}", if path.is_empty() { "." } else { &path }),
        "web_fetch" => format!("Fetch {}", s("url")),
        "web_search" => format!("Search the web: {}", s("query")),
        "request_action" => format!("{} {}", broker::ACTION_TITLE, s("action")),
        "read_email" => format!(
            "Read email {}",
            if s("id").is_empty() {
                s("query")
            } else {
                s("id")
            }
        )
        .trim_end()
        .to_string(),
        other => other.to_string(),
    };
    let mut u = json!({"title": title, "kind": tool_kind(name), "rawInput": args});
    if !path.is_empty() {
        u["locations"] = json!([{"path": resolve(cwd, &path)}]);
    }
    match name {
        "write_file" => {
            u["content"] =
                json!([{"type": "diff", "path": resolve(cwd, &path), "newText": s("content")}])
        }
        "edit_file" => {
            u["content"] = json!([{"type": "diff", "path": resolve(cwd, &path), "oldText": s("old_string"), "newText": s("new_string")}])
        }
        _ => {}
    }
    u
}

// ---- the agent -------------------------------------------------------------------

/// Where the agent's messages to arakun go.
pub type Out = Arc<dyn Fn(Value) + Send + Sync>;

pub(crate) const API: &str = "https://api.cloudflare.com/client/v4";
/// Model calls one prompt may make.
const MAX_STEPS: usize = 200;
/// Tries per model call, for 429s, 5xx and dropped connections.
const TRIES: u32 = 8;
/// Requests per minute Workers AI takes per account and model on standard
/// billing; `ARAKUN_WORKERS_AI_RPM` raises it once Cloudflare grants more.
const RPM: usize = 20;

fn rpm() -> usize {
    std::env::var("ARAKUN_WORKERS_AI_RPM")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(RPM)
}

/// One model's share of the account limit, for every session in the app: when
/// its last requests went out, and until when a 429 asked everyone to hold off.
#[derive(Default)]
struct Lane {
    sent: VecDeque<Instant>,
    hold: Option<Instant>,
}

fn lanes() -> &'static Mutex<HashMap<String, Lane>> {
    static LANES: OnceLock<Mutex<HashMap<String, Lane>>> = OnceLock::new();
    LANES.get_or_init(Default::default)
}

impl Lane {
    /// How long until a request fits in the last minute's budget; when it
    /// fits now, the slot is taken.
    fn take(&mut self, rpm: usize, now: Instant) -> Option<Duration> {
        while self
            .sent
            .front()
            .is_some_and(|&t| now.duration_since(t) >= Duration::from_secs(60))
        {
            self.sent.pop_front();
        }
        if let Some(h) = self.hold.filter(|&h| h > now) {
            return Some(h - now);
        }
        if self.sent.len() >= rpm {
            return Some(self.sent[self.sent.len() - rpm] + Duration::from_secs(60) - now);
        }
        self.sent.push_back(now);
        None
    }
}
/// Takes a slot under `model`'s per-minute limit, or says how long until one frees.
pub(crate) fn lane_wait(model: &str) -> Option<Duration> {
    lock(lanes())
        .entry(model.to_string())
        .or_default()
        .take(rpm(), Instant::now())
}

/// A stream quiet this long is taken for dead.
const STALL: Duration = Duration::from_secs(300);

/// The photos a Claude model takes as they are: these types, up to 5 MB each (base64 here).
const CLAUDE_SEES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];
const CLAUDE_PHOTO_CHARS: usize = 5_000_000;

/// How often an idle Claude session reads its prompt cache to keep it (it lives 5 minutes
/// from the start of the last request), and how many times after a turn.
const WARM_EVERY: Duration = Duration::from_secs(270);
const WARM_TIMES: u32 = 13;

static KEEP_WARM: AtomicBool = AtomicBool::new(true);

/// A headless host turns this off: nobody is reading its replies to answer in a few minutes.
pub fn keep_warm(on: bool) {
    KEEP_WARM.store(on, Ordering::SeqCst);
}

fn keep_warm_on() -> bool {
    KEEP_WARM.load(Ordering::SeqCst) && std::env::var("ARAKUN_KEEP_WARM").map_or(true, |v| v != "0")
}

/// The model that reads attached photos: the ones the session runs on are
/// text-only, so this one lends them its eyes.
pub const VISION_MODEL: &str = "@cf/meta/llama-3.2-11b-vision-instruct";

/// What the vision model is asked, in Spanish: a full account of the photo,
/// because its words are the only way the session's model will ever see it.
const DESCRIBE_PROMPT: &str = "Describe esta imagen con detalle y en español, para alguien que no puede verla: \
qué muestra, cualquier texto que contenga (transcríbelo), la distribución y jerarquía de los elementos, \
los colores (nombra los tonos principales), el estilo y la intención del diseño o la escena. \
Sé concreta y completa; tu descripción es la única forma en que un modelo de texto la verá.";

pub(crate) fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(300)))
            .timeout_recv_body(Some(Duration::from_secs(3600)))
            .http_status_as_error(false)
            // Cloudflare's bot rule turns some default user agents away (error 1010).
            .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
            .build()
            .into()
    })
}

/// Why a model call did not come back with a reply.
pub(crate) enum Failed {
    /// Worth asking again: a 429, a 5xx, the network. Seconds the server asked to wait.
    Retry(String, Option<u64>),
    Fatal(i64, String),
    /// Stopped by the user; what had come so far.
    Cancelled(Box<Reply>),
}

/// What the network thread hands the turn.
pub(crate) enum Net {
    Chunk(Vec<u8>),
    End,
    Failed(Failed),
}

#[derive(Default)]
struct Used {
    input: u64,
    cached: u64,
    output: u64,
    reasoning: u64,
}

/// Text deltas coalesced, so a fast stream doesn't redraw the session per token.
struct Live {
    kind: u8,
    text: String,
    at: Instant,
}

enum Permit {
    Allowed,
    Denied,
    /// Denied, and the turn goes on: the model hears why (headless sessions).
    Declined(String),
    Cancelled,
}

struct Agent {
    out: Out,
    asks: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    next_id: AtomicU64,
    cancel: AtomicBool,
    busy: AtomicBool,
    session: Mutex<Option<Arc<Mutex<Session>>>>,
    /// Goes up when a turn starts or the connection ends: a keep-warm loop from before stops.
    turns: AtomicU64,
}

/// A turn that panicked while it held a lock (`guard`) must not take every later
/// request down with it: the session is then as its last save left it, or a little ahead.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serves one connection: what arakun writes comes in on `rx`, answers go to `out`.
pub fn serve(rx: mpsc::Receiver<String>, out: Out) {
    let agent = Arc::new(Agent {
        out,
        asks: Mutex::default(),
        next_id: AtomicU64::new(1),
        cancel: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        session: Mutex::default(),
        turns: AtomicU64::new(0),
    });
    for line in rx.iter() {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        match msg["method"].as_str() {
            Some(method) => {
                // A request that panics is answered with an error; the connection stays.
                let handled = crate::guard::run("a Workers AI request", || {
                    agent.handle(method, id.clone(), &msg["params"])
                });
                if let (Err(panicked), Some(id)) = (handled, id) {
                    agent.reply(id, Err((-32603, panicked.to_string())));
                }
            }
            None => {
                let waiting = id
                    .as_ref()
                    .and_then(Value::as_u64)
                    .and_then(|n| lock(&agent.asks).remove(&n));
                if let Some(tx) = waiting {
                    let _ = tx.send(msg);
                }
            }
        }
    }
    agent.cancel.store(true, Ordering::SeqCst);
    agent.turns.fetch_add(1, Ordering::SeqCst);
    lock(&agent.asks).clear();
}

fn prompt_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The image blocks of a prompt: `(mime, base64)`, in the order they were named.
fn prompt_images(blocks: &Value) -> Vec<(String, String)> {
    blocks
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let data = b["data"].as_str()?;
            Some((
                b["mimeType"].as_str().unwrap_or("image/jpeg").to_string(),
                data.to_string(),
            ))
        })
        .collect()
}

impl Agent {
    fn send(&self, msg: Value) {
        (self.out)(msg)
    }

    fn reply(&self, id: Value, result: Result<Value, (i64, String)>) {
        self.send(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        });
    }

    fn update(&self, sid: &str, update: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": update}}));
    }

    fn notice(&self, sid: &str, text: &str, error: bool) {
        self.send(json!({"jsonrpc": "2.0", "method": "_arakun/notice", "params": {"sessionId": sid, "text": text, "error": error}}));
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn current(&self) -> Option<Arc<Mutex<Session>>> {
        lock(&self.session).clone()
    }

    /// A request to arakun, answered by the user; `None` once the turn is stopped.
    fn ask(&self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.asks).insert(id, tx);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(v) => return Some(v),
                Err(mpsc::RecvTimeoutError::Timeout) if !self.cancelled() => {}
                _ => {
                    lock(&self.asks).remove(&id);
                    return None;
                }
            }
        }
    }

    fn handle(self: &Arc<Self>, method: &str, id: Option<Value>, params: &Value) {
        let reply = |result| {
            if let Some(id) = id.clone() {
                self.reply(id, result)
            }
        };
        let no_session = || Err((-32002, "no Workers AI session".to_string()));
        match method {
            "initialize" => reply(Ok(json!({
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": true,
                    "sessionCapabilities": {"resume": {}},
                    "promptCapabilities": {"image": true},
                },
                "agentInfo": {"name": "workers-ai", "title": "Workers AI", "version": env!("ARAKUN_VERSION")},
            }))),
            "session/new" => {
                knobs::log_once();
                let cwd = params["cwd"]
                    .as_str()
                    .map(PathBuf::from)
                    .unwrap_or_else(launch::home);
                let s = Session::new(cwd);
                s.save();
                let result = json!({"sessionId": s.id, "configOptions": s.config()});
                *lock(&self.session) = Some(Arc::new(Mutex::new(s)));
                reply(Ok(result))
            }
            // The transcript already shows the history, so loading replays nothing.
            "session/resume" | "session/load" => {
                match params["sessionId"].as_str().and_then(Session::load) {
                    Some(s) => {
                        knobs::log_once();
                        let result = json!({"configOptions": s.config()});
                        *lock(&self.session) = Some(Arc::new(Mutex::new(s)));
                        reply(Ok(result))
                    }
                    None => reply(Err((
                        -32002,
                        "this Workers AI session is not saved on this machine".into(),
                    ))),
                }
            }
            "session/set_config_option" | "session/set_model" => {
                let Some(session) = self.current() else {
                    return reply(no_session());
                };
                let mut s = lock(&session);
                let value = params["value"]
                    .as_str()
                    .or(params["modelId"].as_str())
                    .unwrap_or("");
                match params["configId"].as_str().unwrap_or("model") {
                    "model" => match model(value) {
                        Some(m) => {
                            // Another model starts on its own effort, as a draft does: each
                            // model's default is the level it works best at for its price.
                            s.effort = if m.id == s.model {
                                m.effort(Some(&s.effort))
                            } else {
                                m.default_effort
                            }
                            .into();
                            s.model = m.id.into();
                        }
                        None => {
                            return reply(Err((
                                -32602,
                                format!("Workers AI has no model {value}"),
                            )));
                        }
                    },
                    "reasoning_effort" if s.model().efforts.contains(&value) => {
                        s.effort = value.into()
                    }
                    other => return reply(Err((-32602, format!("cannot set {other} to {value}")))),
                }
                s.save();
                reply(Ok(json!({"configOptions": s.config()})))
            }
            "session/prompt" => {
                let Some(id) = id else { return };
                let Some(session) = self.current() else {
                    return self.reply(id, no_session());
                };
                if self.busy.swap(true, Ordering::SeqCst) {
                    return self.reply(id, Err((-32603, "a turn is already running".into())));
                }
                self.cancel.store(false, Ordering::SeqCst);
                let turn = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
                let text = prompt_text(&params["prompt"]);
                let images = prompt_images(&params["prompt"]);
                let agent = self.clone();
                let spawned = std::thread::Builder::new()
                    .name("workers-ai-turn".into())
                    .spawn(move || {
                        // A turn that panics still answers, and the next prompt can run.
                        let result = crate::guard::run("the Workers AI turn", || {
                            agent.turn(&session, text, images)
                        })
                        .unwrap_or_else(|panicked| Err((-32603, panicked.to_string())));
                        let ended = matches!(&result, Ok(v) if v["stopReason"] == "end_turn");
                        agent.busy.store(false, Ordering::SeqCst);
                        agent.reply(id, result);
                        if ended && keep_warm_on() {
                            agent.keep_warm(&session, turn);
                        }
                    });
                if let Err(e) = spawned {
                    self.busy.store(false, Ordering::SeqCst);
                    eprintln!("arakun: could not start a Workers AI turn: {e}");
                }
            }
            "session/cancel" => self.cancel.store(true, Ordering::SeqCst),
            _ => reply(Err((-32601, format!("method not found: {method}")))),
        }
    }

    /// While the user reads and thinks, the conversation stays in Anthropic's prompt cache: it
    /// lives five minutes from the last request, and a reply that comes later than that pays
    /// to write the whole conversation again (25 times what reading it costs). So after a turn
    /// the same request goes out every 4.5 minutes asking for no output, which reads the cache
    /// and resets its clock, for about an hour; all of those together cost half of one miss.
    /// Runs on the turn's thread, which has nothing else to do, until the next turn starts.
    fn keep_warm(&self, session: &Mutex<Session>, turn: u64) {
        let live = || self.turns.load(Ordering::SeqCst) == turn;
        for n in 0..=WARM_TIMES {
            // The first one goes out at once: the cache's five minutes count from when the
            // last request started, and a long reply has used up part of them.
            let wait = if n == 0 {
                Duration::from_secs(2)
            } else {
                WARM_EVERY
            };
            let asleep = unix_now();
            let until = Instant::now() + wait;
            while Instant::now() < until {
                if !live() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            // The machine slept through it: the cache is gone, and nobody may be coming back.
            if unix_now().saturating_sub(asleep) > wait.as_secs() + 25 {
                return;
            }
            // The request the turn's last call made: the history up to what the model answered.
            let (sid, model, mut body) = {
                let s = lock(session);
                let m = s.model();
                let asked = s
                    .messages
                    .iter()
                    .rposition(|m| m["role"] != "assistant")
                    .map_or(0, |i| i + 1);
                (
                    s.id.clone(),
                    m,
                    request_body(m, s.effort(), &s.messages[..asked]),
                )
            };
            if model.api != Api::Anthropic {
                return;
            }
            let Ok(creds) = endpoint_for(model) else {
                return;
            };
            body["max_tokens"] = json!(0);
            body["stream"] = json!(false);
            let request = ureq::http::Request::builder()
                .method("POST")
                .uri(&creds.url)
                .header("x-api-key", creds.token.as_deref().unwrap_or(""))
                .header("anthropic-version", anthropic::VERSION)
                .header("anthropic-beta", anthropic::BETA)
                .header("Content-Type", "application/json")
                .body(body.to_string());
            let Ok(request) = request else { return };
            let Ok(mut response) = http().run(request) else {
                return;
            };
            let text = response
                .body_mut()
                .with_config()
                .limit(1 << 20)
                .read_to_string()
                .unwrap_or_default();
            if response.status().as_u16() != 200 || !live() {
                if response.status().as_u16() != 200 {
                    eprintln!(
                        "arakun: keeping the prompt cache warm stopped: {} {}",
                        response.status(),
                        text.chars().take(200).collect::<String>()
                    );
                }
                return;
            }
            let usage = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["usage"].as_object().cloned())
                .unwrap_or_default();
            let u = anthropic::usage_of(&usage);
            let n = |v: &Value| v.as_u64().unwrap_or(0);
            let (prompt, cached, written) = (
                n(&u["prompt_tokens"]),
                n(&u["prompt_tokens_details"]["cached_tokens"]),
                n(&u["cache_creation_input_tokens"]),
            );
            let (cost, context) = {
                let mut s = lock(session);
                s.cost += model.cost(prompt.saturating_sub(cached + written), cached, written, 0);
                s.save();
                (s.cost, s.context)
            };
            self.update(
                &sid,
                json!({
                    "sessionUpdate": "usage_update",
                    "used": context,
                    "size": model.context,
                    "cost": {"amount": cost, "currency": "USD"},
                    "_meta": {"model": model.id},
                }),
            );
        }
    }

    fn done(stop: &str, used: &Used) -> Value {
        json!({"stopReason": stop, "usage": {
            "inputTokens": used.input,
            "outputTokens": used.output,
            "cachedReadTokens": used.cached,
            "thoughtTokens": used.reasoning,
        }})
    }

    /// What one attached photo shows, in Spanish, in enough detail for a
    /// text-only model to act on it. One small request to the vision model,
    /// which takes the photo as base64 in `image` — the chat-completions
    /// endpoint does not take image parts, so this goes to `/ai/run/` instead.
    fn describe(
        &self,
        creds: &Endpoint,
        sid: &str,
        _mime: &str,
        data: &str,
    ) -> Result<String, String> {
        self.wait_for_lane(sid, VISION_MODEL).map_err(|e| match e {
            Failed::Retry(why, _) | Failed::Fatal(_, why) => why,
            Failed::Cancelled(_) => "cancelled".to_string(),
        })?;
        let url = creds.url.replace(
            "/ai/v1/chat/completions",
            "/ai/run/@cf/meta/llama-3.2-11b-vision-instruct",
        );
        let body = json!({ "prompt": DESCRIBE_PROMPT, "image": data, "max_tokens": 800 });
        let mut builder = ureq::http::Request::builder()
            .method("POST")
            .uri(&url)
            .header("Content-Type", "application/json");
        if let Some(token) = &creds.token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let request = builder.body(body.to_string()).map_err(|e| e.to_string())?;
        let mut response = http()
            .run(request)
            .map_err(|e| format!("could not reach the API ({e})"))?;
        let text = response
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_string()
            .unwrap_or_default();
        if response.status().as_u16() != 200 {
            return Err(format!(
                "the vision model answered {}: {}",
                response.status(),
                text.trim().chars().take(200).collect::<String>()
            ));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        v["result"]["response"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the vision model said nothing".to_string())
    }

    /// One prompt: model calls and their tools until the model answers without any.
    fn turn(
        &self,
        session: &Mutex<Session>,
        mut text: String,
        images: Vec<(String, String)>,
    ) -> Result<Value, (i64, String)> {
        let (sid, cwd) = {
            let s = lock(session);
            (s.id.clone(), s.cwd.clone())
        };
        let mut used = Used::default();
        if text.trim_start().starts_with("/compact") {
            return Err((
                -32601,
                "Workers AI sessions are not compacted; the models hold 1M tokens".into(),
            ));
        }
        let no_key = |how: String, m: &Model| {
            self.notice(&sid, &how, true);
            Err((
                RpcError::AUTH_REQUIRED,
                format!("{} has no API key", m.api.name()),
            ))
        };
        let first = lock(session).model();
        if let Err(how) = endpoint_for(first) {
            return no_key(how, first);
        }
        // A Claude model sees the photos itself: they travel in the message as images. For the
        // other models (text-only), and for a photo Claude's API would not take, a vision model
        // on Workers AI reads each one and its description travels in the message, where later
        // turns still see it.
        let mut seen: Vec<Value> = Vec::new();
        let mut vision: Option<Result<Endpoint, String>> = None;
        for (n, (mime, data)) in images.iter().enumerate() {
            if first.api == Api::Anthropic
                && CLAUDE_SEES.contains(&mime.as_str())
                && data.len() <= CLAUDE_PHOTO_CHARS
            {
                seen.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}));
                continue;
            }
            let read = match vision.get_or_insert_with(endpoint) {
                Ok(creds) => self.describe(creds, &sid, mime, data),
                Err(how) => Err(how.clone()),
            };
            let what = match read {
                Ok(words) => words,
                Err(e) => {
                    self.notice(
                        &sid,
                        &format!("Photo {} could not be read: {e}", n + 1),
                        false,
                    );
                    format!("(unreadable photo: {e})")
                }
            };
            text.push_str(&format!(
                "\n\n[photo {} — what the vision model sees: {}]",
                n + 1,
                what.trim()
            ));
        }
        {
            let mut s = lock(session);
            // A retried prompt is already there, unanswered.
            let prompt = if seen.is_empty() {
                json!({"role": "user", "content": text})
            } else {
                if !text.trim().is_empty() {
                    seen.push(json!({"type": "text", "text": text}));
                }
                json!({"role": "user", "content": seen})
            };
            if s.messages.last() != Some(&prompt) {
                s.messages.push(prompt);
                s.save();
            }
        }
        let knobs = knobs::get();
        let mut budget = knobs::ReadBudget::default();
        let mut progress = knobs::Progress::default();
        // The rules that wait for an edit also see the ones made from the shell: the working
        // tree is compared before and after each command (only when such a rule is on).
        let watch_tree =
            knobs.adaptive_effort || knobs.self_test || knobs.stop_on_green || knobs.read_budget;
        let mut tree = if watch_tree {
            knobs::tree_mark(&cwd)
        } else {
            None
        };
        for _ in 0..MAX_STEPS {
            if self.cancelled() {
                return Ok(Self::done("cancelled", &used));
            }
            let (body, model) = {
                let mut s = lock(session);
                if let Some(p) = &knobs.prune
                    && knobs::prune(&mut s.messages, p) > 0
                {
                    s.save();
                }
                let m = s.model();
                if s.estimate() + max_output() > m.context {
                    drop(s);
                    self.notice(
                        &sid,
                        &format!("Stopped: the next request would not fit in {}'s context. Start a new session to go on.", m.name),
                        true,
                    );
                    return Ok(Self::done("end_turn", &used));
                }
                let effort = match knobs.reasoning_effort.as_deref() {
                    Some(e) => m.effort(Some(e)),
                    None if knobs.adaptive_effort => m.effort(Some(progress.effort())),
                    None => s.effort(),
                };
                progress.called();
                s.efforts.push(effort.to_string());
                (request_body(m, effort, &s.messages), m)
            };
            // Per call: the session's model can change between two of them.
            let creds = match endpoint_for(model) {
                Ok(c) => c,
                Err(how) => return no_key(how, model),
            };
            let mut reply = match self.call(&sid, &creds, &body) {
                Ok(r) => r,
                Err(Failed::Cancelled(partial)) => {
                    if !partial.content.is_empty() {
                        let mut s = lock(session);
                        s.messages
                            .push(json!({"role": "assistant", "content": partial.content}));
                        s.save();
                    }
                    return Ok(Self::done("cancelled", &used));
                }
                Err(Failed::Fatal(code, msg)) => return Err((code, msg)),
                Err(Failed::Retry(msg, _)) => return Err((-32603, msg)),
            };
            reply.settle();
            let n = |k: &str| {
                reply
                    .usage
                    .as_ref()
                    .and_then(|u| u[k].as_u64())
                    .unwrap_or(0)
            };
            let (prompt, output) = (n("prompt_tokens"), n("completion_tokens"));
            let cached = reply
                .usage
                .as_ref()
                .and_then(|u| u["prompt_tokens_details"]["cached_tokens"].as_u64())
                .unwrap_or(0)
                .min(prompt);
            let written = n("cache_creation_input_tokens").min(prompt - cached);
            used.input += prompt - cached;
            used.cached += cached;
            used.output += output;
            used.reasoning += reply
                .usage
                .as_ref()
                .and_then(|u| u["completion_tokens_details"]["reasoning_tokens"].as_u64())
                .unwrap_or(0);
            let cost = {
                let mut s = lock(session);
                s.cost += model.cost(prompt - cached - written, cached, written, output);
                if prompt > 0 {
                    s.context = prompt + output;
                }
                s.messages.push(reply.message());
                s.save();
                s.cost
            };
            self.update(
                &sid,
                json!({
                    "sessionUpdate": "usage_update",
                    "used": prompt + output,
                    "size": model.context,
                    "cost": {"amount": cost, "currency": "USD"},
                    "_meta": {"model": model.id},
                }),
            );
            if reply.calls.is_empty() {
                let stop = if reply.finish.as_deref() == Some("length") {
                    "max_tokens"
                } else {
                    "end_turn"
                };
                if knobs.self_test && stop == "end_turn" && progress.self_test_due() {
                    let mut s = lock(session);
                    s.messages
                        .push(json!({"role": "user", "content": knobs::SELF_TEST_REMINDER}));
                    s.save();
                    continue;
                }
                return Ok(Self::done(stop, &used));
            }
            let mut halt: Option<&str> = None;
            let mut shell_edit = false;
            let calls: Vec<&Call> = reply.calls.values().collect();
            let mut i = 0;
            while i < calls.len() {
                // A run of read-only calls goes at once when the knob says; results keep their order.
                let n = if knobs.parallel_tools && halt.is_none() {
                    calls[i..]
                        .iter()
                        .take_while(|c| read_only(&c.name))
                        .count()
                        .max(1)
                } else {
                    1
                };
                let group = &calls[i..i + n];
                i += n;
                let results: Vec<(String, Option<&'static str>)> = match halt {
                    Some(why) => group
                        .iter()
                        .map(|_| (format!("(not run: {why})"), None))
                        .collect(),
                    None if n == 1 => vec![self.run_call(&sid, session, &cwd, group[0])],
                    None => std::thread::scope(|scope| {
                        let handles: Vec<_> = group
                            .iter()
                            .map(|c| scope.spawn(|| self.run_call(&sid, session, &cwd, c)))
                            .collect();
                        handles
                            .into_iter()
                            .map(|h| {
                                h.join()
                                    .unwrap_or_else(|_| ("Error: the tool panicked".into(), None))
                            })
                            .collect()
                    }),
                };
                for (call, (content, why)) in group.iter().zip(results) {
                    let ran = halt.is_none() && why.is_none();
                    let mut content = content;
                    if watch_tree
                        && ran
                        && matches!(call.name.as_str(), "bash" | "edit_file" | "write_file")
                    {
                        let now = knobs::tree_mark(&cwd);
                        if call.name == "bash" && now != tree {
                            budget.note_edit();
                            shell_edit = true;
                        }
                        tree = now;
                    }
                    let cmd = (call.name == "bash")
                        .then(|| serde_json::from_str::<Value>(&call.arguments).ok())
                        .flatten();
                    let cmd = cmd.as_ref().and_then(|v| v["command"].as_str());
                    if knobs.read_budget && ran && budget.note_call(&call.name, cmd, content.len())
                    {
                        content.push_str("\n\n");
                        content.push_str(knobs::BUDGET_REMINDER);
                    }
                    if knobs.stop_on_green
                        && ran
                        && let Some(rem) =
                            budget.note_green_test_with_clef(&cwd, &call.name, cmd, &content)
                    {
                        content.push_str("\n\n");
                        content.push_str(&rem);
                    }
                    let mut s = lock(session);
                    s.messages
                        .push(json!({"role": "tool", "tool_call_id": call.id, "content": content}));
                    s.save();
                    drop(s);
                    halt = halt.or(why);
                }
            }
            if knobs.adaptive_effort || knobs.self_test {
                let s = lock(session);
                let tail = s.messages.len().saturating_sub(calls.len());
                let results: Vec<(&str, &str)> = calls
                    .iter()
                    .zip(&s.messages[tail..])
                    .map(|(c, m)| (c.name.as_str(), m["content"].as_str().unwrap_or("")))
                    .collect();
                progress.results(results);
                if shell_edit {
                    progress.tree_changed();
                }
            }
            match halt {
                Some("the user denied an earlier call") => {
                    self.notice(&sid, "Stopped: you denied a tool call.", false);
                    return Ok(Self::done("end_turn", &used));
                }
                Some(_) => return Ok(Self::done("cancelled", &used)),
                None => {}
            }
        }
        Ok(Self::done("max_turn_requests", &used))
    }

    /// Runs one tool call, asking first when it needs to; returns what the model
    /// is told, and why the rest of the calls must not run, if they mustn't.
    fn run_call(
        &self,
        sid: &str,
        session: &Mutex<Session>,
        cwd: &Path,
        call: &Call,
    ) -> (String, Option<&'static str>) {
        let id = call.id.as_str();
        let args = match serde_json::from_str::<Value>(if call.arguments.trim().is_empty() {
            "{}"
        } else {
            &call.arguments
        }) {
            Ok(v) if v.is_object() => v,
            _ => {
                let mut u = json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed"});
                u["rawOutput"] = json!("invalid arguments");
                self.update(sid, u);
                return (
                    format!(
                        "Error: the arguments are not a JSON object: {}",
                        clip_middle(&call.arguments, 2000)
                    ),
                    None,
                );
            }
        };
        if call.name == "request_action" {
            return self.action(sid, id, cwd, &args);
        }
        let mut shown = describe(&call.name, &args, cwd);
        shown["sessionUpdate"] = json!("tool_call_update");
        shown["toolCallId"] = json!(id);
        self.update(sid, shown.clone());
        if let Some(group) = permission_group(&call.name)
            && !lock(session).always.iter().any(|g| g == group)
        {
            match self.permit(sid, Some(session), group, &shown) {
                Permit::Allowed => {}
                Permit::Denied => {
                    self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed", "rawOutput": "Denied"}));
                    return (
                        "The user denied this call.".into(),
                        Some("the user denied an earlier call"),
                    );
                }
                Permit::Declined(why) => {
                    self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed", "rawOutput": "Declined"}));
                    let why = if why.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", why.trim())
                    };
                    return (
                        format!(
                            "The user declined this call{why}. Do not try it another way; go on without it, or say what you could not do."
                        ),
                        None,
                    );
                }
                Permit::Cancelled => {
                    self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed"}));
                    return (
                        "(not run: the turn was stopped)".into(),
                        Some("the turn was stopped"),
                    );
                }
            }
        }
        self.update(
            sid,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "in_progress"}),
        );
        if call.name == "bash" {
            return self.run_command(sid, cwd, id, &args);
        }
        let result = match call.name.as_str() {
            "read_file" => read_file(cwd, &args),
            "write_file" => write_file(cwd, &args),
            "edit_file" => edit_file(cwd, &args),
            "grep" => grep(cwd, &args, &self.cancel),
            "list" => list(cwd, &args),
            "web_fetch" => super::web::fetch(&args).map(|t| clip_middle(&t, tool_chars())),
            "web_search" => super::web::search(&args).map(|t| clip_middle(&t, tool_chars())),
            "read_email"
                if jail()
                    .is_some_and(|j| j.caps.contains(&crate::headless::sandbox::Cap::MailRead)) =>
            {
                broker::read_email(&args)
            }
            other => Err(format!("there is no tool {other}")),
        };
        let (status, text) = match result {
            Ok(text) => ("completed", text),
            Err(e) => ("failed", format!("Error: {e}")),
        };
        let shown_output = if call.name == "read_file" && status == "completed" {
            format!(
                "{} lines",
                text.lines().filter(|l| l.contains('\t')).count()
            )
        } else {
            clip_middle(&text, tool_chars())
        };
        self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": status, "rawOutput": shown_output}));
        // A stopped search stops the turn, like a stopped command: without the
        // halt the calls queued after it still run.
        let halt = (call.name == "grep" && self.cancelled()).then_some("the turn was stopped");
        (text, halt)
    }

    fn run_command(
        &self,
        sid: &str,
        cwd: &Path,
        id: &str,
        args: &Value,
    ) -> (String, Option<&'static str>) {
        let Some(command) = args["command"].as_str().filter(|c| !c.trim().is_empty()) else {
            self.update(
                sid,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed"}),
            );
            return ("Error: command is missing".into(), None);
        };
        let secs = args["timeout_secs"].as_u64().unwrap_or(600).clamp(1, 3600);
        let (mut buffer, mut sent, mut at) = (String::new(), 0usize, Instant::now());
        let mut spool = knobs::get().bash_tail.then(|| Spool::new(id));
        let flush = |buffer: &mut String, sent: &mut usize| {
            if buffer.is_empty() || *sent >= LIVE_CHARS {
                buffer.clear();
                return;
            }
            let mut data = std::mem::take(buffer);
            *sent += data.len();
            if *sent >= LIVE_CHARS {
                data.push_str("\n[arakun: the rest of the output is not shown here]\n");
            }
            self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "_meta": {"terminal_output": {"data": data}}}));
        };
        let ran = run_bash(
            cwd,
            command,
            Duration::from_secs(secs),
            &self.cancel,
            |text| {
                if let Some(s) = spool.as_mut() {
                    s.push(text);
                }
                buffer.push_str(text);
                if at.elapsed() > Duration::from_millis(100) {
                    flush(&mut buffer, &mut sent);
                    at = Instant::now();
                }
            },
        );
        flush(&mut buffer, &mut sent);
        let ran = match ran {
            Ok(r) => r,
            Err(e) => {
                self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed", "rawOutput": e}));
                return (format!("Error: {e}"), None);
            }
        };
        let ok = ran.stopped.is_none() && ran.exit == Some(0);
        let mut u = json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": if ok { "completed" } else { "failed" }});
        if let Some(code) = ran.exit {
            u["_meta"] = json!({"terminal_exit": {"exit_code": code}});
        }
        self.update(sid, u);
        let mut text = match spool.as_ref().and_then(Spool::saved) {
            Some((path, total)) => knobs::clip_tail(&ran.output, tool_chars(), &path, total),
            None => clip_middle(&ran.output, tool_chars()),
        };
        if text.trim().is_empty() {
            text = "(no output)".into();
        }
        let end = match (&ran.stopped, ran.exit) {
            (Some(why), _) => format!("[{why}]"),
            (None, Some(code)) => format!("[exit code {code}]"),
            (None, None) => "[killed by a signal]".into(),
        };
        let halt = (ran.stopped.as_deref() == Some("stopped")).then_some("the turn was stopped");
        (format!("{}\n{end}", text.trim_end()), halt)
    }

    /// `request_action`: checked, shown, asked every time, and run by the host
    /// only on a yes.
    fn action(
        &self,
        sid: &str,
        id: &str,
        cwd: &Path,
        args: &Value,
    ) -> (String, Option<&'static str>) {
        let failed = |msg: String| {
            self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed", "rawOutput": msg}));
            (format!("Error: {msg}"), None)
        };
        let Some(jail) = jail() else {
            return failed("there is no tool request_action".into());
        };
        let prepared = match broker::prepare(args, &jail) {
            Ok(v) => v,
            Err(e) => return failed(e),
        };
        let mut shown = describe("request_action", &prepared, cwd);
        shown["sessionUpdate"] = json!("tool_call_update");
        shown["toolCallId"] = json!(id);
        self.update(sid, shown.clone());
        match self.permit(sid, None, "action", &shown) {
            Permit::Allowed => {}
            Permit::Denied => {
                self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed", "rawOutput": "Denied"}));
                return (
                    "The user denied this action; nothing was done.".into(),
                    Some("the user denied an earlier call"),
                );
            }
            Permit::Declined(why) => {
                self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed", "rawOutput": "Declined"}));
                let why = if why.trim().is_empty() {
                    String::new()
                } else {
                    format!(" ({})", why.trim())
                };
                return (
                    format!(
                        "The user declined this action{why}; nothing was done. Do not try it another way."
                    ),
                    None,
                );
            }
            Permit::Cancelled => {
                self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "failed"}));
                return (
                    "(not run: the turn was stopped)".into(),
                    Some("the turn was stopped"),
                );
            }
        }
        self.update(
            sid,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "in_progress"}),
        );
        let (status, text) = match broker::execute(&prepared, &jail.workspace) {
            Ok(out) => ("completed", format!("Approved and done.\n{out}")),
            Err(e) => ("failed", format!("Approved, but it failed: {e}")),
        };
        self.update(sid, json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": status, "rawOutput": clip_middle(&text, tool_chars())}));
        (text, None)
    }

    /// Asks the user about a call; `session` takes an "always" answer, which
    /// actions never offer.
    fn permit(
        &self,
        sid: &str,
        session: Option<&Mutex<Session>>,
        group: &str,
        shown: &Value,
    ) -> Permit {
        let always = if group == "bash" {
            "Always allow commands"
        } else {
            "Always allow edits"
        };
        let mut tool = shown.clone();
        if let Some(o) = tool.as_object_mut() {
            o.remove("sessionUpdate");
        }
        let mut options = vec![json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"})];
        if session.is_some() {
            options.push(json!({"optionId": "always", "name": always, "kind": "allow_always"}));
        }
        options.push(json!({"optionId": "deny", "name": "Deny", "kind": "reject_once"}));
        let params = json!({
            "sessionId": sid,
            "toolCall": tool,
            "options": options,
        });
        let Some(answer) = self.ask("session/request_permission", params) else {
            return Permit::Cancelled;
        };
        let outcome = &answer["result"]["outcome"];
        match (outcome["outcome"].as_str(), outcome["optionId"].as_str()) {
            (Some("selected"), Some("allow")) => Permit::Allowed,
            (Some("selected"), Some("always")) if session.is_some() => {
                let mut s = lock(session.unwrap());
                s.always.push(group.to_string());
                s.save();
                Permit::Allowed
            }
            (Some("selected"), _) if answer["result"]["_meta"]["arakun"]["continue"] == true => {
                Permit::Declined(
                    answer["result"]["_meta"]["arakun"]["why"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            }
            (Some("selected"), _) => Permit::Denied,
            _ => Permit::Cancelled,
        }
    }

    /// One model call with its retries; a 429 or a 5xx waits and asks again.
    fn call(&self, sid: &str, creds: &Endpoint, body: &Value) -> Result<Reply, Failed> {
        let lane = body["model"].as_str().unwrap_or_default().to_string();
        let who = creds.api.name();
        let mut attempt = 0;
        loop {
            attempt += 1;
            // The per-minute limit the sessions share is Workers AI's.
            if creds.api == Api::Chat {
                self.wait_for_lane(sid, &lane)?;
            }
            let (why, wait) = match self.stream(sid, creds, body) {
                Err(Failed::Retry(why, wait)) => (why, wait),
                other => return other,
            };
            if let Some(w) = wait {
                let mut lanes = lock(lanes());
                let l = lanes.entry(lane.clone()).or_default();
                let until = Instant::now() + Duration::from_secs(w.clamp(1, 300));
                l.hold = Some(l.hold.map_or(until, |h| h.max(until)));
            }
            if attempt >= TRIES {
                return Err(Failed::Retry(
                    format!("{who}: {why} (gave up after {TRIES} tries)"),
                    None,
                ));
            }
            let wait = wait
                .unwrap_or([5, 10, 20, 40, 60][(attempt as usize - 1).min(4)])
                .clamp(1, 300);
            if attempt == 1 {
                self.notice(
                    sid,
                    &format!("{who}: {why}. Retrying, with longer waits each time."),
                    false,
                );
            }
            let title = format!(
                "Retrying {who}, attempt {attempt} of {}. {why}; waiting {wait}s.",
                TRIES - 1
            );
            self.update(
                sid,
                json!({"sessionUpdate": "session_info_update", "_meta": {"jetbrains": {"air": {"sessionFailure": {"severity": "warning", "title": title}}}}}),
            );
            let until = Instant::now() + Duration::from_secs(wait);
            while Instant::now() < until {
                if self.cancelled() {
                    return Err(Failed::Cancelled(Box::default()));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    /// Waits for room under the model's per-minute limit, which every session
    /// on that model shares, so they take turns instead of meeting 429s.
    fn wait_for_lane(&self, sid: &str, lane: &str) -> Result<(), Failed> {
        let mut told = false;
        loop {
            let Some(wait) = lock(lanes())
                .entry(lane.to_string())
                .or_default()
                .take(rpm(), Instant::now())
            else {
                return Ok(());
            };
            if !told && wait > Duration::from_secs(1) {
                told = true;
                let title = format!(
                    "Waiting {}s for a free Workers AI slot ({} requests a minute per model).",
                    wait.as_secs().max(1),
                    rpm()
                );
                self.update(
                    sid,
                    json!({"sessionUpdate": "session_info_update", "_meta": {"jetbrains": {"air": {"sessionFailure": {"severity": "warning", "title": title}}}}}),
                );
            }
            let until = Instant::now() + wait.min(Duration::from_secs(1));
            while Instant::now() < until {
                if self.cancelled() {
                    return Err(Failed::Cancelled(Box::default()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// One streamed request. The network runs on a thread of its own, so a stop
    /// doesn't wait for the next byte.
    fn stream(&self, sid: &str, creds: &Endpoint, body: &Value) -> Result<Reply, Failed> {
        let mut request = ureq::http::Request::builder()
            .method("POST")
            .uri(&creds.url);
        match (&creds.token, creds.api) {
            (Some(token), Api::Chat) => {
                request = request.header("Authorization", format!("Bearer {token}"))
            }
            (token, Api::Anthropic) => {
                request = request
                    .header("x-api-key", token.as_deref().unwrap_or(""))
                    .header("anthropic-version", anthropic::VERSION)
                    .header("anthropic-beta", anthropic::BETA)
            }
            (None, Api::Chat) => {}
        }
        // Anthropic's events, read into the Chat Completions payloads the reply takes.
        let mut events = (creds.api == Api::Anthropic).then(anthropic::Stream::default);
        let request = request
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body.to_string())
            .map_err(|e| Failed::Fatal(-32603, e.to_string()))?;
        let (tx, rx) = mpsc::channel::<Net>();
        std::thread::Builder::new()
            .name("workers-ai-net".into())
            .spawn(move || fetch(request, &tx))
            .map_err(|e| Failed::Fatal(-32603, e.to_string()))?;
        let mut sse = Sse::default();
        let mut reply = Reply::default();
        let mut live = Live {
            kind: 0,
            text: String::new(),
            at: Instant::now(),
        };
        let mut heard = Instant::now();
        let mut shown = false;
        'read: loop {
            if self.cancelled() {
                self.flush(sid, &mut live);
                return Err(Failed::Cancelled(Box::new(reply)));
            }
            let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Net::Chunk(bytes)) => bytes,
                Ok(Net::End) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Net::Failed(f)) => {
                    self.flush(sid, &mut live);
                    return Err(self.broke_off(sid, f, shown));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if heard.elapsed() > STALL {
                        self.flush(sid, &mut live);
                        return Err(self.broke_off(
                            sid,
                            Failed::Retry("no answer for 5 minutes".into(), None),
                            shown,
                        ));
                    }
                    continue;
                }
            };
            heard = Instant::now();
            let mut payloads = Vec::new();
            for data in sse.feed(&chunk) {
                match &mut events {
                    None => payloads.push(Ok(data)),
                    Some(ev) => match ev.take(&data) {
                        Ok(chunks) => payloads.extend(chunks.into_iter().map(Ok)),
                        Err(e) => payloads.push(Err(e)),
                    },
                }
            }
            for data in payloads {
                let deltas = match data.and_then(|data| reply.take(&data)) {
                    Ok(d) => d,
                    Err(e) => {
                        self.flush(sid, &mut live);
                        return Err(self.broke_off(sid, Failed::Retry(e, None), shown));
                    }
                };
                for d in deltas {
                    shown = true;
                    match d {
                        Delta::Reasoning(t) => self.live(sid, &mut live, 2, &t),
                        Delta::Content(t) => self.live(sid, &mut live, 1, &t),
                        Delta::Call(index) => {
                            self.flush(sid, &mut live);
                            let c = &reply.calls[&index];
                            self.update(
                                sid,
                                json!({"sessionUpdate": "tool_call", "toolCallId": c.id, "title": c.name, "kind": tool_kind(&c.name), "status": "pending"}),
                            );
                        }
                    }
                }
                if reply.done {
                    break 'read;
                }
            }
        }
        self.flush(sid, &mut live);
        if !reply.done && reply.finish.is_none() {
            return Err(self.broke_off(
                sid,
                Failed::Retry("the reply broke off".into(), None),
                shown,
            ));
        }
        if let Some(ev) = &events {
            reply.native = ev.blocks();
            if ev.dropped > 0 {
                eprintln!(
                    "arakun: Anthropic left {} thinking blocks of the history out of this call (the history was edited)",
                    ev.dropped
                );
            }
        }
        Ok(reply)
    }

    /// A reply that failed after part of it showed says so before it is asked again.
    fn broke_off(&self, sid: &str, f: Failed, shown: bool) -> Failed {
        if shown && let Failed::Retry(why, _) = &f {
            self.notice(
                sid,
                &format!("The model's reply failed: {why}; asking again."),
                false,
            );
        }
        f
    }

    fn live(&self, sid: &str, live: &mut Live, kind: u8, text: &str) {
        if live.kind != kind {
            self.flush(sid, live);
            live.kind = kind;
        }
        live.text.push_str(text);
        if live.at.elapsed() > Duration::from_millis(60) {
            self.flush(sid, live);
        }
    }

    fn flush(&self, sid: &str, live: &mut Live) {
        live.at = Instant::now();
        if live.text.is_empty() {
            return;
        }
        let kind = if live.kind == 2 {
            "agent_thought_chunk"
        } else {
            "agent_message_chunk"
        };
        let text = std::mem::take(&mut live.text);
        self.update(
            sid,
            json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}),
        );
    }
}

/// Sends the request and hands its body to `tx` as it comes.
pub(crate) fn fetch(request: ureq::http::Request<String>, tx: &mpsc::Sender<Net>) {
    let response = match http().run(request) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Net::Failed(Failed::Retry(
                format!("could not reach the API ({e})"),
                None,
            )));
            return;
        }
    };
    let status = response.status().as_u16();
    if status != 200 {
        let wait = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()?.trim().parse::<u64>().ok());
        let text = response
            .into_body()
            .with_config()
            .limit(1 << 20)
            .read_to_string()
            .unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| api_error(&v))
            .unwrap_or_else(|| text.trim().chars().take(300).collect());
        let why = match status {
            429 => format!("rate limited (429) {detail}").trim().to_string(),
            _ => format!("HTTP {status}: {detail}"),
        };
        let failed = match status {
            408 | 429 | 500..=599 => Failed::Retry(why, wait),
            401 | 403 => Failed::Fatal(
                RpcError::AUTH_REQUIRED,
                format!("The API refused the key: {why}"),
            ),
            _ => Failed::Fatal(-32603, format!("The API answered: {why}")),
        };
        let _ = tx.send(Net::Failed(failed));
        return;
    }
    let mut reader = response.into_body().into_reader();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(Net::End);
                return;
            }
            Ok(n) => {
                if tx.send(Net::Chunk(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Net::Failed(Failed::Retry(
                    format!("the stream broke ({e})"),
                    None,
                )));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_lines_survive_any_split() {
        let stream = "data: {\"a\":1}\r\n\ndata: {\"b\":\"ñ\"}\n: keepalive\n\ndata: [DONE]\n";
        let whole = Sse::default().feed(stream.as_bytes());
        assert_eq!(whole, vec!["{\"a\":1}", "{\"b\":\"ñ\"}", "[DONE]"]);
        for at in 0..stream.len() {
            let mut sse = Sse::default();
            let mut got = sse.feed(&stream.as_bytes()[..at]);
            got.extend(sse.feed(&stream.as_bytes()[at..]));
            assert_eq!(got, whole, "split at {at}");
        }
    }

    #[test]
    fn reply_assembles_reasoning_text_and_tool_calls() {
        let mut r = Reply::default();
        let chunks = [
            json!({"choices": [{"delta": {"reasoning_content": "Let me ", "reasoning": "Let me "}}]}),
            json!({"choices": [{"delta": {"reasoning_content": "look.", "reasoning": "look."}}]}),
            json!({"choices": [{"delta": {"content": "Checking."}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "bash", "arguments": "{\"comm"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "id": "c2", "function": {"name": "list", "arguments": ""}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "and\":\"ls\"}"}}]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": {"cached_tokens": 60}}}),
        ];
        let mut deltas = Vec::new();
        for c in &chunks {
            deltas.extend(r.take(&c.to_string()).unwrap());
        }
        r.take("[DONE]").unwrap();
        assert!(r.done);
        assert_eq!(r.reasoning, "Let me look.");
        assert_eq!(r.content, "Checking.");
        assert_eq!(
            deltas,
            vec![
                Delta::Reasoning("Let me ".into()),
                Delta::Reasoning("look.".into()),
                Delta::Content("Checking.".into()),
                Delta::Call(0),
                Delta::Call(1),
            ]
        );
        assert_eq!(
            r.calls[&0],
            Call {
                id: "c1".into(),
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into()
            }
        );
        assert_eq!(r.finish.as_deref(), Some("tool_calls"));
        assert_eq!(
            r.usage.as_ref().unwrap()["prompt_tokens_details"]["cached_tokens"],
            60
        );
        let m = r.message();
        assert_eq!(m["reasoning_content"], "Let me look.");
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
        assert_eq!(m["tool_calls"][1]["function"]["arguments"], "{}");
        assert_eq!(m["tool_calls"][1]["type"], "function");
        assert!(
            r.take(&json!({"errors": [{"message": "Capacity temporarily exceeded"}]}).to_string())
                .is_err()
        );
    }

    #[test]
    fn a_lane_lets_rpm_through_a_minute_then_waits() {
        let t0 = Instant::now();
        let mut lane = Lane::default();
        for i in 0..3 {
            assert_eq!(lane.take(3, t0 + Duration::from_secs(i)), None);
        }
        assert_eq!(
            lane.take(3, t0 + Duration::from_secs(10)),
            Some(Duration::from_secs(50))
        );
        assert_eq!(lane.take(3, t0 + Duration::from_secs(60)), None);
        lane.hold = Some(t0 + Duration::from_secs(90));
        assert_eq!(
            lane.take(3, t0 + Duration::from_secs(70)),
            Some(Duration::from_secs(20))
        );
    }

    #[test]
    fn calls_without_ids_get_one() {
        let mut r = Reply::default();
        r.take(&json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"name": "grep", "arguments": "{}"}}]}}]}).to_string()).unwrap();
        r.settle();
        assert!(r.calls[&0].id.starts_with("call_"));
        let m = Reply {
            content: "hi".into(),
            ..Reply::default()
        }
        .message();
        assert_eq!(m, json!({"role": "assistant", "content": "hi"}));
    }

    #[test]
    fn null_content_becomes_empty_only_where_the_model_refuses_it() {
        let msgs = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": null, "tool_calls": []}),
        ];
        let oss = model("@cf/openai/gpt-oss-120b").unwrap();
        assert_eq!(
            request_body(oss, "medium", &msgs)["messages"][1]["content"],
            ""
        );
        assert!(request_body(&MODELS[0], "high", &msgs)["messages"][1]["content"].is_null());
    }

    #[test]
    fn body_takes_the_model_default_effort() {
        let ds = model("@cf/deepseek-ai/deepseek-v4.1-flash-1017").unwrap();
        let glm = model("@cf/zai-org/glm-5.3").unwrap();
        assert_eq!(ds.effort(None), "high");
        assert_eq!(glm.effort(None), "max");
        assert_eq!(glm.effort(Some("none")), "max");
        assert_eq!(ds.effort(Some("none")), "none");
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let b = request_body(glm, glm.effort(Some("bogus")), &msgs);
        assert_eq!(b["model"], "@cf/zai-org/glm-5.3");
        assert_eq!(b["reasoning_effort"], "max");
        assert_eq!(b["max_completion_tokens"], max_output());
        assert!(b.get("max_tokens").is_none());
        assert_eq!(b["stream"], true);
        assert_eq!(b["stream_options"]["include_usage"], true);
        let names: Vec<&str> = b["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "bash",
                "read_file",
                "write_file",
                "edit_file",
                "grep",
                "list",
                "web_fetch",
                "web_search"
            ]
        );
        assert!((ds.cost(1_000_000, 0, 0, 1_000_000) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_claude_model_gets_a_messages_request_and_other_models_no_claude_blocks() {
        let sonnet = model("claude-sonnet-5-5").unwrap();
        assert_eq!(sonnet.effort(None), "high");
        assert_eq!(sonnet.effort(Some("medium")), "medium");
        let blocks = json!([{"type": "thinking", "thinking": "hm", "signature": "s"}, {"type": "text", "text": "hi"}]);
        let messages = [
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi", anthropic::NATIVE: blocks}),
            json!({"role": "user", "content": "again"}),
        ];
        let b = request_body(sonnet, "medium", &messages);
        assert_eq!(b["model"], "claude-sonnet-5-5");
        assert_eq!(b["output_config"]["effort"], "medium");
        assert_eq!(b["system"][0]["text"], "sys");
        assert_eq!(b["messages"][1]["content"][0]["signature"], "s");
        assert_eq!(b["tools"][0]["name"], "bash");
        assert!(b["tools"][0]["input_schema"].is_object());
        // read 100k from the cache, wrote 10k, 5k fresh, 2k out
        let usd = sonnet.cost(5_000, 100_000, 10_000, 2_000);
        assert!((usd - (0.01 + 0.01 + 0.025 + 0.02)).abs() < 1e-9, "{usd}");
        let ds = request_body(&MODELS[0], "high", &messages);
        assert!(ds["messages"][2].get(anthropic::NATIVE).is_none());
        assert_eq!(ds["messages"][2]["content"], "hi");
    }

    #[test]
    fn a_claude_model_gets_the_photo_and_other_models_a_note() {
        let photo = json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{RED_PNG}")}});
        let messages = [
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": [photo, {"type": "text", "text": "what is this?"}]}),
        ];
        let b = request_body(model("claude-sonnet-5-5").unwrap(), "high", &messages);
        let blocks = &b["messages"][0]["content"];
        assert_eq!(
            blocks[0]["source"],
            json!({"type": "base64", "media_type": "image/png", "data": RED_PNG})
        );
        assert_eq!(blocks[1]["text"], "what is this?");
        let ds = request_body(&MODELS[0], "high", &messages);
        assert_eq!(
            ds["messages"][1]["content"],
            "[a photo was attached here; this model cannot see it]\n\nwhat is this?"
        );
        // the estimate counts the photo as a photo, not as its text
        let mut s = Session::new(PathBuf::from("/tmp"));
        let before = s.estimate();
        s.messages.push(json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "x".repeat(3_000_000)}}]}));
        assert_eq!(s.estimate() - before, 1_600);
    }

    /// A 32x32 red square.
    const RED_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAJ0lEQVR42u3NsQkAAAjAsP7/tF7hIASyp6lTCQQCgUAgEAgEgi/BAjLD/C5w/SM9AAAAAElFTkSuQmCC";

    /// Claude sees a photo in the prompt itself (about a cent):
    /// `cargo test anthropic_sees_photos_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn anthropic_sees_photos_live() {
        let dir = std::env::temp_dir().join(format!("arakun-photo-live-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        let (out_tx, out_rx) = mpsc::channel::<Value>();
        let out: Out = Arc::new(move |v| {
            let _ = out_tx.send(v);
        });
        std::thread::spawn(move || serve(rx, out));
        let send = |v: Value| tx.send(v.to_string()).unwrap();
        send(
            json!({"jsonrpc": "2.0", "id": 1002, "method": "session/new", "params": {"cwd": dir}}),
        );
        let (mut sid, mut said) = (String::new(), String::new());
        loop {
            let v = out_rx
                .recv_timeout(Duration::from_secs(300))
                .expect("the agent went quiet");
            let answer = |n: u64| v.get("method").is_none() && v["id"] == n;
            if answer(1002) {
                sid = v["result"]["sessionId"].as_str().unwrap().to_string();
                send(
                    json!({"jsonrpc": "2.0", "id": 1004, "method": "session/set_config_option", "params": {"sessionId": sid, "configId": "model", "value": "claude-sonnet-5-5"}}),
                );
            }
            if answer(1004) {
                let prompt = json!([{"type": "image", "mimeType": "image/png", "data": RED_PNG}, {"type": "text", "text": "Which single color fills this image? Answer with one word, no tools."}]);
                send(
                    json!({"jsonrpc": "2.0", "id": 1003, "method": "session/prompt", "params": {"sessionId": sid, "prompt": prompt}}),
                );
            }
            if v["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                said.push_str(
                    v["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap_or(""),
                );
            }
            if v["method"] == "_arakun/notice" {
                println!("notice {}", v["params"]["text"]);
            }
            if answer(1003) {
                println!("said: {said}");
                assert_eq!(v["result"]["stopReason"], "end_turn", "{v}");
                assert!(said.to_lowercase().contains("red"), "{said}");
                break;
            }
        }
        if let Some(p) = Session::path(&sid) {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drafts_offer_the_picked_model_efforts() {
        let o = draft_options(&[]);
        assert_eq!(o[0].current, MODELS[0].id);
        assert_eq!(o[1].current, "high");
        let o = draft_options(&[
            (Setting::Model, "@cf/zai-org/glm-5.3".into()),
            (Setting::Effort, "none".into()),
        ]);
        assert_eq!(o[1].current, "max");
        assert_eq!(
            o[1].options
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "high", "max"]
        );
        assert_eq!(o[1].category.as_deref(), Some("thought_level"));
    }

    #[test]
    fn interrupted_histories_get_their_tool_answers() {
        let call = |id: &str| json!({"id": id, "type": "function", "function": {"name": "bash", "arguments": "{}"}});
        let mut m = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": "go"}),
            json!({"role": "assistant", "content": null, "tool_calls": [call("a"), call("b")]}),
            json!({"role": "tool", "tool_call_id": "a", "content": "ok"}),
            json!({"role": "user", "content": "Arakun restarted"}),
            json!({"role": "assistant", "content": null, "tool_calls": [call("c")]}),
        ];
        repair(&mut m);
        let roles: Vec<String> = m
            .iter()
            .map(|x| {
                format!(
                    "{}:{}",
                    x["role"].as_str().unwrap(),
                    x["tool_call_id"].as_str().unwrap_or("")
                )
            })
            .collect();
        assert_eq!(
            roles,
            [
                "system:",
                "user:",
                "assistant:",
                "tool:a",
                "tool:b",
                "user:",
                "assistant:",
                "tool:c"
            ]
        );
        assert_eq!(m[4]["content"], INTERRUPTED);
        let again = m.clone();
        repair(&mut m);
        assert_eq!(m, again);
    }

    #[test]
    fn sessions_round_trip_through_their_file() {
        let mut s = Session::new(PathBuf::from("/tmp"));
        s.messages.push(json!({"role": "user", "content": "hi"}));
        s.effort = "max".into();
        let back: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(back.messages, s.messages);
        assert_eq!(back.effort(), "max");
        assert!(
            back.messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("/tmp")
        );
        assert!(Session::path("../etc").is_none());
    }

    #[test]
    fn edits_must_match_once() {
        assert_eq!(
            apply_edit("a b a", "b", "c", false),
            Ok(("a c a".into(), 1))
        );
        assert!(
            apply_edit("a b a", "a", "c", false)
                .unwrap_err()
                .contains("2 times")
        );
        assert_eq!(apply_edit("a b a", "a", "c", true), Ok(("c b c".into(), 2)));
        assert!(
            apply_edit("a", "x", "y", false)
                .unwrap_err()
                .contains("not in the file")
        );
        assert!(apply_edit("a", "", "y", false).is_err());
        assert!(apply_edit("a", "a", "a", false).is_err());
    }

    #[test]
    fn file_tools_work_in_the_cwd() {
        let dir = std::env::temp_dir().join(format!("arakun-wai-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            write_file(
                &dir,
                &json!({"path": "src/x.txt", "content": "one\ntwo\nthree\n"})
            )
            .unwrap()
            .starts_with("Created")
        );
        assert_eq!(
            read_file(&dir, &json!({"path": "src/x.txt", "offset": 2, "limit": 1})).unwrap(),
            "     2\ttwo\n… 1 more lines; read on with offset=3\n"
        );
        edit_file(
            &dir,
            &json!({"path": "src/x.txt", "old_string": "two", "new_string": "2"}),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("src/x.txt")).unwrap(),
            "one\n2\nthree\n"
        );
        assert!(
            edit_file(
                &dir,
                &json!({"path": "src/x.txt", "old_string": "zzz", "new_string": "y"})
            )
            .is_err()
        );
        assert_eq!(list(&dir, &json!({})).unwrap(), "src/");
        assert!(
            grep(&dir, &json!({"pattern": "thr"}), &AtomicBool::new(false))
                .unwrap()
                .contains("x.txt:3:three")
        );
        assert_eq!(
            grep(
                &dir,
                &json!({"pattern": "nothing-here"}),
                &AtomicBool::new(false)
            )
            .unwrap(),
            "No matches."
        );
        // A stopped turn stops its search.
        assert_eq!(
            grep(&dir, &json!({"pattern": "thr"}), &AtomicBool::new(true)),
            Err("the search stopped".to_string())
        );
        let ran = run_bash(
            &dir,
            "echo out; echo err >&2; exit 3",
            Duration::from_secs(10),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(ran.exit, Some(3));
        assert!(ran.output.contains("out") && ran.output.contains("err"));
        let slow = run_bash(
            &dir,
            "sleep 5",
            Duration::from_secs(1),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(slow.stopped.unwrap().starts_with("timed out"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_instructions_prefer_agents_md_and_are_capped() {
        let dir = std::env::temp_dir().join(format!("arakun-wai-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(project_instructions(&dir), None);
        std::fs::write(dir.join("CLAUDE.md"), "claude rules\n").unwrap();
        assert_eq!(
            project_instructions(&dir),
            Some(("CLAUDE.md", "claude rules".into()))
        );
        std::fs::write(dir.join("AGENTS.md"), "ñ".repeat(INSTRUCTIONS_BYTES)).unwrap();
        let (name, text) = project_instructions(&dir).unwrap();
        assert_eq!(name, "AGENTS.md");
        assert!(text.len() < INSTRUCTIONS_BYTES + 100 && text.ends_with("cut off here]"));
        assert!(system_prompt(&dir).contains("# Project instructions (AGENTS.md)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clips_keep_head_and_tail() {
        let text = format!("{}{}", "a".repeat(30_000), "z".repeat(30_000));
        let c = clip_middle(&text, tool_chars());
        assert!(c.len() < tool_chars() + 100);
        assert!(c.starts_with("aaa") && c.ends_with("zzz") && c.contains("bytes cut"));
        assert_eq!(clip_middle("ñañaña", 6).chars().next(), Some('ñ'));
    }

    #[test]
    fn credentials_prefer_the_environment() {
        let auth = json!({
            "opencode-go": {"type": "api", "key": "go"},
            "cloudflare-workers-ai": {"type": "api", "key": " tok ", "metadata": {"accountId": "acc"}},
        });
        let c = |t: Option<&str>, a: Option<&str>, auth: Option<&Value>, f: Option<&str>| {
            pick_credentials(t.map(str::to_string), a.map(str::to_string), auth, f)
        };
        assert_eq!(
            c(None, None, Some(&auth), None),
            Some(Credentials {
                account: "acc".into(),
                token: "tok".into()
            })
        );
        assert_eq!(
            c(Some("env"), Some("eacc"), Some(&auth), Some("file")),
            Some(Credentials {
                account: "eacc".into(),
                token: "env".into()
            })
        );
        let bare = json!({"workers-ai": {"type": "api", "key": "k"}});
        assert_eq!(
            c(None, None, Some(&bare), Some("file\n")),
            Some(Credentials {
                account: "file".into(),
                token: "k".into()
            })
        );
        assert_eq!(c(None, None, Some(&bare), None), None);
        assert_eq!(c(None, Some("a"), None, None), None);
    }

    /// One real tool loop against the API (fractions of a cent):
    /// `cargo test workers_ai_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn workers_ai_live() {
        live_loop(None, "high");
    }

    /// The same loop on Claude Sonnet 5.5 through Anthropic's API (about a cent):
    /// `cargo test anthropic_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn anthropic_live() {
        live_loop(Some("claude-sonnet-5-5"), "high");
    }

    /// Writes one history as the bench's bridge got it and as this loop now sends it to
    /// Anthropic, for `arakun-bench` to compare the two translations:
    /// `ARAKUN_EQ_OUT=/tmp/eq.json cargo test request_for_the_bridge -- --ignored`.
    #[test]
    #[ignore]
    fn request_for_the_bridge() {
        let Some(out) = std::env::var_os("ARAKUN_EQ_OUT") else {
            return;
        };
        let call = |id: &str, cmd: &str| json!({"id": id, "type": "function", "function": {"name": "bash", "arguments": json!({"command": cmd, "timeout": 30}).to_string()}});
        let b1 = json!([
            {"type": "thinking", "thinking": "look first", "signature": "sig1"},
            {"type": "text", "text": "Let me look."},
            {"type": "tool_use", "id": "toolu_a", "name": "bash", "input": {"command": "ls", "timeout": 30}},
            {"type": "tool_use", "id": "toolu_b", "name": "bash", "input": {"command": "git status", "timeout": 30}},
        ]);
        let b2 = json!([{"type": "thinking", "thinking": "done", "signature": "sig2"}, {"type": "text", "text": "All good."}]);
        let messages = vec![
            json!({"role": "system", "content": system_prompt(Path::new("/app"))}),
            json!({"role": "user", "content": "Fix the bug.\n\nDetails: ñandú"}),
            json!({"role": "assistant", "content": "Let me look.", "reasoning_content": "look first", "tool_calls": [call("toolu_a", "ls"), call("toolu_b", "git status")], anthropic::NATIVE: b1}),
            json!({"role": "tool", "tool_call_id": "toolu_a", "content": "a.py\nb.py"}),
            json!({"role": "tool", "tool_call_id": "toolu_b", "content": ""}),
            json!({"role": "assistant", "content": null, "tool_calls": [call("call_x", "cat a.py")]}),
            json!({"role": "tool", "tool_call_id": "call_x", "content": "print(1)"}),
            json!({"role": "assistant", "content": "All good.", "reasoning_content": "done", anthropic::NATIVE: b2}),
            json!({"role": "user", "content": knobs::SELF_TEST_REMINDER}),
        ];
        let chat = request_body(&MODELS[0], "high", &messages);
        let native = request_body(model("claude-sonnet-5-5").unwrap(), "high", &messages);
        let kept = json!({"c-toolu_a": b1, "text:All good.": b2});
        std::fs::write(
            out,
            json!({"chat": chat, "native": native, "kept": kept}).to_string(),
        )
        .unwrap();
    }

    fn live_loop(pick: Option<&str>, effort: &str) {
        let dir = std::env::temp_dir().join(format!("arakun-wai-live-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        let (out_tx, out_rx) = mpsc::channel::<Value>();
        let out: Out = Arc::new(move |v| {
            let _ = out_tx.send(v);
        });
        std::thread::spawn(move || serve(rx, out));
        let send = |v: Value| tx.send(v.to_string()).unwrap();
        send(json!({"jsonrpc": "2.0", "id": 1001, "method": "initialize", "params": {}}));
        send(
            json!({"jsonrpc": "2.0", "id": 1002, "method": "session/new", "params": {"cwd": dir}}),
        );
        let mut sid = String::new();
        let mut kinds: Vec<String> = Vec::new();
        let mut prompted = false;
        loop {
            let v = out_rx
                .recv_timeout(Duration::from_secs(600))
                .expect("the agent went quiet");
            // Its own requests (permissions) count from 1.
            let answer = |n: u64| v.get("method").is_none() && v["id"] == n;
            if answer(1002) {
                sid = v["result"]["sessionId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{v}"))
                    .to_string();
                match pick {
                    Some(m) => send(
                        json!({"jsonrpc": "2.0", "id": 1004, "method": "session/set_config_option", "params": {"sessionId": sid, "configId": "model", "value": m}}),
                    ),
                    None => assert_eq!(v["result"]["configOptions"][1]["currentValue"], effort),
                }
            }
            if answer(1004) {
                assert_eq!(
                    v["result"]["configOptions"][1]["currentValue"], effort,
                    "{v}"
                );
            }
            if !sid.is_empty() && !prompted && (pick.is_none() || answer(1004)) {
                prompted = true;
                let text = "Create hello.txt containing the word hi with write_file, then run `cat hello.txt` with bash, then answer in one short sentence.";
                send(
                    json!({"jsonrpc": "2.0", "id": 1003, "method": "session/prompt", "params": {"sessionId": sid, "prompt": [{"type": "text", "text": text}]}}),
                );
            }
            if v["method"] == "session/request_permission" {
                kinds.push(format!("permission {}", v["params"]["toolCall"]["title"]));
                send(
                    json!({"jsonrpc": "2.0", "id": v["id"], "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}),
                );
            }
            if v["method"] == "_arakun/notice" {
                kinds.push(format!("notice {}", v["params"]["text"]));
            }
            if let Some(kind) = v["params"]["update"]["sessionUpdate"].as_str() {
                let detail = match kind {
                    "tool_call" | "tool_call_update" => format!(
                        " {} {}",
                        v["params"]["update"]["title"], v["params"]["update"]["status"]
                    ),
                    "usage_update" => format!(
                        " {} tok ${:.5}",
                        v["params"]["update"]["used"],
                        v["params"]["update"]["cost"]["amount"]
                            .as_f64()
                            .unwrap_or(0.)
                    ),
                    _ => String::new(),
                };
                let line = format!("{kind}{detail}");
                if kinds.last() != Some(&line) {
                    kinds.push(line);
                }
            }
            if answer(1003) {
                println!("{}", kinds.join("\n"));
                println!("result: {}", v);
                assert_eq!(v["result"]["stopReason"], "end_turn", "{v}");
                if pick.is_some() {
                    // the idle session reads its cache to keep it: a fraction of a cent, not a rewrite
                    let before = Session::load(&sid).unwrap().cost;
                    let warm = loop {
                        let v = out_rx
                            .recv_timeout(Duration::from_secs(60))
                            .expect("no keep-warm request");
                        if v["params"]["update"]["sessionUpdate"] == "usage_update" {
                            break v["params"]["update"]["cost"]["amount"].as_f64().unwrap();
                        }
                    };
                    println!("keep-warm cost ${:.5}", warm - before);
                    assert!(warm > before && warm - before < 0.002, "{before} -> {warm}");
                }
                break;
            }
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("hello.txt"))
                .unwrap()
                .trim(),
            "hi"
        );
        let saved = Session::load(&sid).unwrap();
        println!(
            "messages: {}",
            saved
                .messages
                .iter()
                .map(|m| m["role"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if pick.is_some() {
            // every reply kept its own blocks, and the calls after the first read the cache
            let kept = saved
                .messages
                .iter()
                .filter(|m| m["role"] == "assistant")
                .all(|m| m[anthropic::NATIVE].is_array());
            assert!(kept, "an assistant message lost its blocks");
            println!("cost ${:.5}, efforts {:?}", saved.cost, saved.efforts);
            assert!(saved.cost > 0.0);
        } else {
            assert!(
                saved
                    .messages
                    .iter()
                    .any(|m| m["reasoning_content"].is_string())
            );
        }
        if let Some(p) = Session::path(&sid) {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_session_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = std::env::temp_dir().join(format!("arakun-wai-private-{}", uuid_v4()));
        let path = dir.join("workers-ai").join("s.json");
        let session = Session::new(dir.clone());
        session.save_to(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.join("workers-ai")), 0o700);
        // A session saved before this was readable by all; the next save fixes it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        session.save_to(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
