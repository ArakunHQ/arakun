//! Droid's native protocol: `droid exec --input-format stream-jsonrpc
//! --output-format stream-jsonrpc`, the one Factory's own SDK speaks.
//!
//! Compared to ACP it carries more of what Droid knows: Factory credits and
//! token usage, context-window stats, the model catalog, reasoning effort,
//! autonomy level and spec mode as separate settings, AskUser questionnaires and
//! spec-mode plans. Everything maps onto the same [`Thread`] entries, so the
//! transcript view and the saved transcripts don't care which protocol ran.
//!
//! One process holds one session, so requests carry no session id. A turn is
//! `droid.add_user_message` (answered at once) followed by streamed
//! `droid.session_notification`s and finally `agent_turn_completed`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::attach;
use super::super::rpc::{Connection, RpcError};
use super::{
    Entry, FailedAt, PermissionOption, PermissionRequest, PlanEntry, SessionMeta, Thread,
    ThreadEvent, ThreadStatus, ToolCall, ToolContent, ToolStatus, recount, str_of,
};

/// Which protocol a [`Thread`] speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Acp,
    /// Droid's `stream-jsonrpc`.
    Native,
}

/// The session settings the composer shows: native Droid's four, and what Claude's
/// ACP config options map to (mode, model, effort and fast mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Setting {
    Model,
    Effort,
    Autonomy,
    Mode,
    /// Claude's fast mode; native Droid has none.
    Fast,
}

impl Setting {
    pub const ALL: [Setting; 4] = [
        Setting::Mode,
        Setting::Autonomy,
        Setting::Model,
        Setting::Effort,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Setting::Model => "Model",
            Setting::Effort => "Reasoning",
            Setting::Autonomy => "Autonomy",
            Setting::Mode => "Mode",
            Setting::Fast => "Fast mode",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Setting::Model => "modelId",
            Setting::Effort => "reasoningEffort",
            Setting::Autonomy => "autonomyLevel",
            Setting::Mode => "interactionMode",
            Setting::Fast => "fastMode",
        }
    }

    /// The ACP config option category (or id) this setting is, if ACP has it.
    pub fn acp_category(self) -> Option<&'static str> {
        Some(match self {
            Setting::Model => "model",
            Setting::Effort => "thought_level",
            Setting::Mode => "mode",
            Setting::Fast => "fast",
            Setting::Autonomy => return None,
        })
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "modelId" => Setting::Model,
            "reasoningEffort" => Setting::Effort,
            "autonomyLevel" => Setting::Autonomy,
            "interactionMode" => Setting::Mode,
            "fastMode" => Setting::Fast,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingOption {
    pub id: String,
    pub name: String,
    /// A short note shown next to the name (the model's credit multiplier).
    pub detail: Option<String>,
    /// What picking it means, under the name.
    pub description: Option<String>,
    /// Why it can't be picked, if it can't.
    pub disabled: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NativeSettings {
    pub model_id: String,
    pub reasoning_effort: String,
    /// off, low, medium or high.
    pub autonomy_level: String,
    /// auto or spec.
    pub interaction_mode: String,
    #[serde(default)]
    pub autonomy_levels: Vec<String>,
}

impl NativeSettings {
    fn from_json(v: &Value) -> Option<Self> {
        let model_id = str_of(v, "modelId")?;
        Some(NativeSettings {
            model_id,
            reasoning_effort: str_of(v, "reasoningEffort").unwrap_or_default(),
            autonomy_level: str_of(v, "autonomyLevel").unwrap_or_default(),
            interaction_mode: str_of(v, "interactionMode").unwrap_or_else(|| "auto".into()),
            autonomy_levels: v["availableAutonomyLevels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    pub fn get(&self, s: Setting) -> &str {
        match s {
            Setting::Model => &self.model_id,
            Setting::Effort => &self.reasoning_effort,
            Setting::Autonomy => &self.autonomy_level,
            Setting::Mode => &self.interaction_mode,
            Setting::Fast => "",
        }
    }

    pub fn set(&mut self, s: Setting, value: &str) {
        let slot = match s {
            Setting::Model => &mut self.model_id,
            Setting::Effort => &mut self.reasoning_effort,
            Setting::Autonomy => &mut self.autonomy_level,
            Setting::Mode => &mut self.interaction_mode,
            Setting::Fast => return,
        };
        *slot = value.to_string();
    }
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// Credits per token relative to the standard rate.
    pub multiplier: Option<f64>,
    pub deprecated: bool,
    pub disabled: Option<String>,
}

impl ModelInfo {
    pub fn from_json(m: &Value) -> Option<Self> {
        let id = str_of(m, "id").or_else(|| str_of(m, "modelId"))?;
        let name = str_of(m, "displayName").unwrap_or_else(|| id.clone());
        Some(ModelInfo {
            short_name: str_of(m, "shortDisplayName").unwrap_or_else(|| name.clone()),
            name,
            efforts: m["supportedReasoningEfforts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            default_effort: str_of(m, "defaultReasoningEffort"),
            multiplier: m["tokenMultiplier"].as_f64(),
            deprecated: m["deprecated"].as_bool().unwrap_or(false),
            disabled: m["disabled"]
                .as_bool()
                .unwrap_or(false)
                .then(|| str_of(m, "disabledReason").unwrap_or_else(|| "Not available".into())),
            id,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Question {
    /// 1-based, as Droid numbers them.
    pub index: u64,
    pub topic: String,
    pub question: String,
    pub options: Vec<String>,
    pub multi: bool,
}

/// An AskUser questionnaire the agent is waiting on.
#[derive(Clone, Debug)]
pub struct AskUserRequest {
    rpc_id: Value,
    pub tool_call_id: String,
    pub questions: Vec<Question>,
    /// ACP form elicitation (Claude's AskUserQuestion): per question, its field
    /// and the free-text field beside it. Empty for native Droid.
    pub fields: Vec<(String, Option<String>)>,
    /// The subagent session asking, when it is not this one.
    pub session: Option<String>,
}

/// A questionnaire as a server words it (`phone::wire::question`), for a
/// session that runs there.
pub(super) fn remote_question(v: &Value) -> Option<AskUserRequest> {
    let text = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    let questions = v["questions"].as_array()?;
    Some(AskUserRequest {
        rpc_id: Value::Null,
        tool_call_id: text(v, "tool_call_id"),
        questions: questions
            .iter()
            .map(|q| Question {
                index: q["index"].as_u64().unwrap_or(0),
                topic: text(q, "topic"),
                question: text(q, "question"),
                options: q["options"]
                    .as_array()
                    .map(|o| {
                        o.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                multi: q["multi"].as_bool().unwrap_or(false),
            })
            .collect(),
        fields: Vec::new(),
        session: None,
    })
}

impl AskUserRequest {
    /// An ACP `elicitation/create` form as questions: each select field (or plain
    /// text field) is one, and a `<field>_custom` text field rides along as its
    /// free-text answer. `None` for forms this card cannot show.
    pub fn from_form(rpc_id: Value, params: &Value) -> Option<Self> {
        if params["mode"].as_str().is_some_and(|m| m != "form") {
            return None;
        }
        let props = params["requestedSchema"]["properties"].as_object()?;
        let message = str_of(params, "message").unwrap_or_default();
        let single = props.keys().filter(|k| !k.ends_with("_custom")).count() == 1;
        let (mut questions, mut fields) = (Vec::new(), Vec::new());
        for (key, field) in props {
            if key.ends_with("_custom") && props.contains_key(key.trim_end_matches("_custom")) {
                continue;
            }
            let (choices, multi) = match (
                field["oneOf"].as_array(),
                field["items"]["anyOf"].as_array(),
            ) {
                (Some(c), _) => (c.clone(), false),
                (None, Some(c)) => (c.clone(), true),
                _ if field["type"] == "string" => (Vec::new(), false),
                _ => return None,
            };
            let options = choices
                .iter()
                .filter_map(|c| str_of(c, "const").or_else(|| str_of(c, "title")))
                .collect();
            let question = str_of(field, "description")
                .filter(|_| !single)
                .unwrap_or_else(|| message.clone());
            questions.push(Question {
                index: questions.len() as u64 + 1,
                topic: str_of(field, "title").unwrap_or_default(),
                question,
                options,
                multi,
            });
            let custom = format!("{key}_custom");
            fields.push((key.clone(), props.contains_key(&custom).then_some(custom)));
        }
        (!questions.is_empty()).then(|| Self {
            rpc_id,
            tool_call_id: str_of(params, "toolCallId").unwrap_or_default(),
            questions,
            fields,
            session: None,
        })
    }
}

/// The card's answers ("A, B, my own words") as an elicitation's form content:
/// picked options in the field, the rest in its free-text companion.
fn form_content(req: &AskUserRequest, answers: &[String]) -> Value {
    let mut content = serde_json::Map::new();
    for ((q, (key, custom)), answer) in req.questions.iter().zip(&req.fields).zip(answers) {
        let (picked, typed): (Vec<&str>, Vec<&str>) = answer
            .split(", ")
            .partition(|part| q.options.iter().any(|o| o == part));
        let typed = typed.join(", ");
        match (q.multi, picked.first(), custom) {
            (true, _, _) => {
                content.insert(key.clone(), json!(picked));
            }
            (false, Some(first), _) => {
                content.insert(key.clone(), json!(first));
            }
            (false, None, None) => {
                content.insert(key.clone(), json!(typed));
                continue;
            }
            (false, None, Some(_)) => {}
        }
        if let Some(custom) = custom
            && !typed.is_empty()
        {
            content.insert(custom.clone(), json!(typed));
        }
    }
    Value::Object(content)
}

/// The plan attached to a spec-mode exit request.
#[derive(Clone, Debug)]
pub struct SpecPlan {
    pub title: String,
    pub plan: String,
}

/// A rewind being confirmed: going back to before a prompt forks the session
/// there and puts the files the agent touched since back as they were.
#[derive(Clone, Debug)]
pub struct RewindPlan {
    /// Index of the prompt's entry.
    pub entry: usize,
    pub message_id: String,
    pub text: String,
    /// `RewindFileSnapshot`s, sent back unchanged to restore them.
    pub restore: Vec<Value>,
    /// `RewindFileCreation`s: files created since, which get deleted.
    pub delete: Vec<Value>,
    /// Files Droid no longer has a snapshot of, with the reason.
    pub evicted: Vec<(String, String)>,
    pub loading: bool,
    pub running: bool,
}

impl RewindPlan {
    pub fn restore_paths(&self) -> impl Iterator<Item = &str> {
        self.restore.iter().filter_map(|f| f["filePath"].as_str())
    }

    pub fn delete_paths(&self) -> impl Iterator<Item = &str> {
        self.delete.iter().filter_map(|f| f["filePath"].as_str())
    }
}

#[derive(Default)]
pub(super) struct State {
    pub settings: Option<NativeSettings>,
    pub models: Vec<ModelInfo>,
    /// Streaming text blocks: (message id, block index) -> entry.
    pub blocks: HashMap<(String, u64), usize>,
    pub questions: VecDeque<AskUserRequest>,
    /// Settings asked for before the session existed.
    pub want: Vec<(Setting, String)>,
    /// Droid's working state: idle, thinking, executing_tool, compacting_conversation, ...
    pub working: String,
    /// Set when an `error` notification already explained a failing turn.
    pub turn_error: bool,
    /// A rename asked for before the session was loaded.
    pub want_title: Option<String>,
    /// Fork or compact asked for before the session was loaded.
    pub deferred: Vec<Deferred>,
    pub rewind: Option<RewindPlan>,
    /// Context used (percent) when Droid started compacting on its own.
    pub compact_before: Option<u64>,
    /// arakun asked for the compaction in progress, and marks it itself.
    pub compacting: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Deferred {
    Fork,
    Compact,
}

fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "arakun".into())
}

/// What a setting can be set to, given the session settings and Droid's model catalog.
pub fn setting_options(
    settings: &NativeSettings,
    models: &[ModelInfo],
    s: Setting,
) -> (String, Vec<SettingOption>) {
    let current = settings.get(s).to_string();
    let plain = |ids: &[String], name: fn(&str) -> String| {
        ids.iter()
            .map(|id| SettingOption {
                id: id.clone(),
                name: name(id),
                detail: None,
                description: setting_note(s, id).map(str::to_string),
                disabled: None,
            })
            .collect::<Vec<_>>()
    };
    let options = match s {
        Setting::Model => models
            .iter()
            .filter(|m| !m.deprecated || m.id == current)
            .map(|m| SettingOption {
                id: m.id.clone(),
                name: m.name.clone(),
                detail: m.multiplier.map(|x| format!("×{}", fmt_multiplier(x))),
                description: None,
                disabled: m.disabled.clone(),
            })
            .collect(),
        Setting::Effort => {
            let efforts = models
                .iter()
                .find(|m| m.id == settings.model_id)
                .map(|m| m.efforts.clone())
                .unwrap_or_default();
            if efforts.is_empty() {
                plain(std::slice::from_ref(&current), effort_name)
            } else {
                plain(&efforts, effort_name)
            }
        }
        Setting::Autonomy => {
            let levels = if settings.autonomy_levels.is_empty() {
                ["off", "low", "medium", "high"].map(String::from).to_vec()
            } else {
                settings.autonomy_levels.clone()
            };
            plain(&levels, autonomy_name)
        }
        Setting::Mode => plain(&["auto".into(), "spec".into()], mode_name),
        Setting::Fast => Vec::new(),
    };
    (current, options)
}

/// The label a chip shows for a setting's current value.
pub fn setting_label(settings: &NativeSettings, models: &[ModelInfo], s: Setting) -> String {
    let (current, options) = setting_options(settings, models, s);
    match s {
        Setting::Model => models
            .iter()
            .find(|m| m.id == current)
            .map_or(current.clone(), |m| m.short_name.clone()),
        _ => options
            .iter()
            .find(|o| o.id == current)
            .map_or_else(|| current.clone(), |o| o.name.clone()),
    }
}

/// Sets `s` the way `droid.update_session_settings` would take it: a model that
/// can't do the current reasoning effort falls back to its default effort.
pub fn apply_setting(settings: &mut NativeSettings, models: &[ModelInfo], s: Setting, value: &str) {
    settings.set(s, value);
    if s == Setting::Model
        && let Some(m) = models.iter().find(|m| m.id == value)
        && !m.efforts.is_empty()
        && !m.efforts.contains(&settings.reasoning_effort)
        && let Some(effort) = m
            .default_effort
            .clone()
            .or_else(|| m.efforts.first().cloned())
    {
        settings.reasoning_effort = effort;
    }
}

/// What a Droid mode or autonomy level lets it do, for the menus.
fn setting_note(s: Setting, id: &str) -> Option<&'static str> {
    Some(match (s, id) {
        (Setting::Mode, "auto") => "Works on the task straight away",
        (Setting::Mode, "spec") => "Writes a plan first and waits for your approval",
        (Setting::Autonomy, "off") => "Reads freely, asks before any edit or command",
        (Setting::Autonomy, "low") => "Edits files, asks before most commands",
        (Setting::Autonomy, "medium") => "Also runs reversible commands, like installs and builds",
        (Setting::Autonomy, "high") => "Runs any command, pushes and deploys included",
        _ => return None,
    })
}

pub fn effort_name(id: &str) -> String {
    match id {
        "none" | "off" => "Off".into(),
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "Extra high".into(),
        "max" => "Max".into(),
        "ultra" => "Ultra".into(),
        other => other.to_string(),
    }
}

fn autonomy_name(id: &str) -> String {
    match id {
        "off" => "Ask first".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        other => other.to_string(),
    }
}

fn mode_name(id: &str) -> String {
    match id {
        "auto" => "Normal".into(),
        "spec" => "Spec".into(),
        other => other.to_string(),
    }
}

/// Text of a tool result: Droid sends a string, or content blocks.
fn result_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| str_of(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `TodoWrite` input: "1. [in_progress] Do x\n2. [pending] Do y", or a list.
fn parse_todos(input: &Value) -> Vec<PlanEntry> {
    match &input["todos"] {
        Value::String(s) => s
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let rest = line
                    .split_once(". ")
                    .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
                    .map_or(line, |(_, r)| r);
                let rest = rest.strip_prefix("- ").unwrap_or(rest);
                let (status, content) = match rest.strip_prefix('[').and_then(|r| r.split_once(']'))
                {
                    Some((s, c)) => (s.trim().to_string(), c.trim().to_string()),
                    None => ("pending".to_string(), rest.to_string()),
                };
                (!content.is_empty()).then_some(PlanEntry {
                    content,
                    priority: String::new(),
                    status,
                })
            })
            .collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(|t| {
                Some(PlanEntry {
                    content: str_of(t, "content").or_else(|| str_of(t, "text"))?,
                    priority: str_of(t, "priority").unwrap_or_default(),
                    status: str_of(t, "status").unwrap_or_else(|| "pending".into()),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn new_tool(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
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
        exit_code: None,
        parent: None,
        tool_name: None,
    }
}

/// Fills in what a Droid tool call means in ACP's vocabulary (kind, title,
/// locations, diffs), so the transcript view labels it like any other.
fn describe_tool(t: &mut ToolCall, name: &str, input: &Value) {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let path = s("file_path")
        .or_else(|| s("path"))
        .or_else(|| s("directory_path"));
    t.locations = path.iter().map(PathBuf::from).collect();
    t.raw_input = Some(input.clone());
    let (kind, title) = match name {
        "Read" => ("read", format!("Read {}", path.clone().unwrap_or_default())),
        "LS" => ("read", format!("List {}", path.clone().unwrap_or_default())),
        "Grep" => (
            "search",
            format!("Grep {}", s("pattern").unwrap_or_default()),
        ),
        "Glob" => (
            "search",
            format!(
                "Glob {}",
                match &input["patterns"] {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    Value::String(p) => p.clone(),
                    _ => String::new(),
                }
            ),
        ),
        "Execute" | "Bash" => ("execute", s("command").unwrap_or_else(|| name.to_string())),
        "Create" | "Write" => (
            "edit",
            format!("Write {}", path.clone().unwrap_or_default()),
        ),
        "Edit" | "MultiEdit" | "ApplyPatch" => {
            ("edit", format!("Edit {}", path.clone().unwrap_or_default()))
        }
        "FetchUrl" => ("fetch", s("url").unwrap_or_else(|| name.to_string())),
        "WebSearch" => ("fetch", s("query").unwrap_or_else(|| name.to_string())),
        "ExitSpecMode" => (
            "switch_mode",
            s("title")
                .or_else(|| s("titleHint"))
                .or_else(|| {
                    s("plan").and_then(|p| {
                        p.lines()
                            .map(str::trim)
                            .find(|l| !l.is_empty())
                            .map(str::to_string)
                    })
                })
                .filter(|t| !t.trim().is_empty())
                .map_or_else(|| "Plan".to_string(), |t| format!("Plan: {t}")),
        ),
        "AskUser" => {
            let first = s("questionnaire")
                .and_then(|q| {
                    q.lines().find_map(|l| {
                        l.split_once("[question]")
                            .map(|(_, r)| r.trim().to_string())
                    })
                })
                .unwrap_or_default();
            ("other", format!("Ask {first}").trim().to_string())
        }
        "Task" => (
            "other",
            format!(
                "Task {}",
                s("description")
                    .or_else(|| s("subagent_type"))
                    .unwrap_or_default()
            ),
        ),
        _ => ("other", name.to_string()),
    };
    t.kind = kind.into();
    t.title = title.trim().to_string();
    t.content = match name {
        "Create" | "Write" => path
            .map(|p| ToolContent::Diff {
                path: p.into(),
                old_text: None,
                new_text: s("content").unwrap_or_default(),
            })
            .into_iter()
            .collect(),
        "Edit" => path
            .map(|p| ToolContent::Diff {
                path: p.into(),
                old_text: s("old_str"),
                new_text: s("new_str").unwrap_or_default(),
            })
            .into_iter()
            .collect(),
        "MultiEdit" => input["edits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                Some(ToolContent::Diff {
                    path: path.clone()?.into(),
                    old_text: str_of(e, "old_str"),
                    new_text: str_of(e, "new_str").unwrap_or_default(),
                })
            })
            .collect(),
        "ApplyPatch" => s("patch")
            .or_else(|| s("input"))
            .map(|text| ToolContent::Text { text })
            .into_iter()
            .collect(),
        _ => std::mem::take(&mut t.content),
    };
    recount(t);
}

fn finish_tool(t: &mut ToolCall, content: &Value, is_error: bool) {
    t.status = if is_error {
        ToolStatus::Failed
    } else {
        ToolStatus::Completed
    };
    let text = result_text(content);
    if !text.is_empty() {
        t.raw_output = Some(text);
    }
}

/// Rebuilds the transcript from `droid.load_session` messages.
fn history(messages: &[Value]) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut tools: HashMap<String, usize> = HashMap::new();
    for m in messages {
        if m["visibility"].as_str() == Some("llm_only") {
            continue;
        }
        let at = m["createdAt"].as_u64().map_or(0, |ms| ms / 1000);
        let role = m["role"].as_str().unwrap_or("");
        let blocks = m["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if role == "user" {
            let mut text: String = blocks
                .iter()
                .filter(|b| b["type"].as_str() == Some("text"))
                .filter_map(|b| str_of(b, "text"))
                .collect::<Vec<_>>()
                .join("\n");
            crate::agent::chart::strip_hint(&mut text);
            if !text.trim().is_empty() && !text.trim_start().starts_with("<system-reminder>") {
                entries.push(Entry::User {
                    text,
                    at,
                    elapsed_ms: None,
                    message_id: str_of(m, "id"),
                    tokens: None,
                    shell_changes: Vec::new(),
                });
            }
        }
        for b in blocks {
            match (role, b["type"].as_str()) {
                ("assistant", Some("text")) => {
                    let text = str_of(b, "text").unwrap_or_default();
                    if !text.trim().is_empty() {
                        entries.push(Entry::Agent { text });
                    }
                }
                ("assistant", Some("thinking")) => {
                    let text = str_of(b, "thinking").unwrap_or_default();
                    if !text.trim().is_empty() {
                        entries.push(Entry::Thought { text });
                    }
                }
                ("assistant", Some("tool_use")) => {
                    let id = str_of(b, "id").unwrap_or_default();
                    let name = str_of(b, "name").unwrap_or_default();
                    if name == "TodoWrite" {
                        let plan = parse_todos(&b["input"]);
                        if !plan.is_empty() {
                            entries.push(Entry::Plan { entries: plan });
                        }
                        continue;
                    }
                    let mut t = new_tool(&id);
                    describe_tool(&mut t, &name, &b["input"]);
                    t.status = ToolStatus::Completed;
                    tools.insert(id, entries.len());
                    entries.push(Entry::Tool(t));
                }
                (_, Some("tool_result")) => {
                    let id = str_of(b, "toolUseId")
                        .or_else(|| str_of(b, "tool_use_id"))
                        .unwrap_or_default();
                    if let Some(&ix) = tools.get(&id)
                        && let Some(Entry::Tool(t)) = entries.get_mut(ix)
                    {
                        let is_error = b["isError"].as_bool().or(b["is_error"].as_bool());
                        finish_tool(t, &b["content"], is_error.unwrap_or(false));
                    }
                }
                _ => {}
            }
        }
    }
    entries
}

fn turn_notice(reason: &str) -> Option<(&'static str, bool)> {
    Some(match reason {
        "completed" | "spec_handoff" => return None,
        "cancelled" => ("Stopped", false),
        "permission_rejected" => ("Stopped: you denied a tool call.", false),
        "process_exit" => ("Droid exited during the turn.", true),
        "model_usage_exhausted" => (
            "Stopped: your Factory usage for this model is exhausted.",
            true,
        ),
        "model_authentication_failed" => (
            "Stopped: Droid could not authenticate with the model provider.",
            true,
        ),
        "model_request_rejected" => ("Stopped: the model provider rejected the request.", true),
        "model_provider_unreachable" | "model_provider_unavailable" => (
            "Stopped: the model provider is unavailable right now.",
            true,
        ),
        r if r.starts_with("structured_output") => ("Stopped: structured output failed.", true),
        _ => ("The turn failed.", true),
    })
}

impl Thread {
    // ---- public native API ------------------------------------------------

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn native_settings(&self) -> Option<&NativeSettings> {
        self.native.settings.as_ref()
    }

    /// The models Droid offers, disabled ones included.
    pub fn model_catalog(&self) -> &[ModelInfo] {
        &self.native.models
    }

    /// The current value of a native setting and what it can be changed to.
    pub fn setting(&self, s: Setting) -> Option<(String, Vec<SettingOption>)> {
        if self.backend != Backend::Native {
            return self.acp_setting(s);
        }
        if s == Setting::Fast {
            return None;
        }
        let settings = self.native.settings.as_ref()?;
        Some(setting_options(settings, &self.native.models, s))
    }

    /// The label a chip shows for a setting's current value.
    pub fn setting_label(&self, s: Setting) -> Option<String> {
        if self.backend != Backend::Native {
            return self.acp_setting_label(s);
        }
        if s == Setting::Fast {
            return None;
        }
        let settings = self.native.settings.as_ref()?;
        Some(setting_label(settings, &self.native.models, s))
    }

    pub fn set_setting(&mut self, s: Setting, value: &str, cx: &mut Context<Self>) {
        if self.backend != Backend::Native {
            return self.acp_set_setting(s, value, cx);
        }
        if s == Setting::Fast {
            return;
        }
        if self.session_id.is_none() || self.native.settings.is_none() {
            self.native.want.retain(|(w, _)| *w != s);
            self.native.want.push((s, value.to_string()));
            return;
        }
        let Some(settings) = self.native.settings.as_mut() else {
            return;
        };
        if settings.get(s) == value {
            return;
        }
        settings.set(s, value);
        let mut params = json!({ s.key(): value });
        // A model that can't do the current effort would be rejected; take its default.
        if s == Setting::Model
            && let Some(m) = self.native.models.iter().find(|m| m.id == value)
            && !m.efforts.is_empty()
            && !m.efforts.contains(&settings.reasoning_effort)
            && let Some(effort) = m
                .default_effort
                .clone()
                .or_else(|| m.efforts.first().cloned())
        {
            settings.reasoning_effort = effort.clone();
            params["reasoningEffort"] = json!(effort);
        }
        self.meta_changed(cx);
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let request = conn.request("droid.update_session_settings", params);
        cx.spawn(async move |this, cx| {
            if let Err(e) = request.await {
                let _ = this.update(cx, |t, cx| {
                    t.notice(
                        format!("Changing {} failed: {e}", s.label().to_lowercase()),
                        true,
                        cx,
                    );
                    t.native_refresh_settings(cx);
                });
            }
        })
        .detach();
    }

    pub fn pending_question(&self) -> Option<&AskUserRequest> {
        self.native.questions.front()
    }

    /// Answers the pending AskUser questionnaire, one answer per question.
    pub fn answer_question(&mut self, answers: Vec<String>, cx: &mut Context<Self>) {
        let Some(req) = self.native.questions.pop_front() else {
            return;
        };
        if self.remote {
            cx.emit(ThreadEvent::Remote(super::RemoteAction::Answer(answers)));
            cx.notify();
            return;
        }
        let reply = if req.fields.is_empty() {
            let answers: Vec<Value> = req
                .questions
                .iter()
                .zip(answers)
                .map(|(q, a)| json!({"index": q.index, "question": q.question, "answer": a}))
                .collect();
            json!({ "answers": answers })
        } else {
            json!({"action": "accept", "content": form_content(&req, &answers)})
        };
        self.reply_question(req, reply, cx);
    }

    /// Dismisses the pending questionnaire without answering.
    pub fn skip_question(&mut self, cx: &mut Context<Self>) {
        let Some(req) = self.native.questions.pop_front() else {
            return;
        };
        if self.remote {
            cx.emit(ThreadEvent::Remote(super::RemoteAction::SkipQuestion));
            cx.notify();
            return;
        }
        // A declined form tells Claude the user skipped; it goes on.
        let reply = if req.fields.is_empty() {
            json!({"answers": [], "cancelled": true})
        } else {
            json!({"action": "decline"})
        };
        self.reply_question(req, reply, cx);
    }

    /// Sends the answer to a questionnaire. A subagent's thread holds a copy
    /// of what its session asks the parent, as with permission requests;
    /// either answers it, and the other drops its copy.
    fn reply_question(&mut self, req: AskUserRequest, reply: Value, cx: &mut Context<Self>) {
        match self.parent.clone() {
            Some(parent) => {
                let id = req.rpc_id.clone();
                cx.defer(move |cx| {
                    let _ = parent.update(cx, |p, cx| p.answer_question_for_child(&id, reply, cx));
                });
            }
            None => {
                if let Some(conn) = &self.conn {
                    conn.respond(req.rpc_id.clone(), reply);
                }
                self.forget_question_in_child(&req, cx);
            }
        }
        self.after_input(&req.tool_call_id, cx);
    }

    /// The user answered a subagent's questionnaire in the subagent's own view.
    fn answer_question_for_child(&mut self, rpc_id: &Value, reply: Value, cx: &mut Context<Self>) {
        let Some(at) = self
            .native
            .questions
            .iter()
            .position(|q| &q.rpc_id == rpc_id)
        else {
            return;
        };
        let Some(req) = self.native.questions.remove(at) else {
            return;
        };
        if let Some(conn) = &self.conn {
            conn.respond(req.rpc_id.clone(), reply);
        }
        self.after_input(&req.tool_call_id, cx);
    }

    /// Drops the subagent's copy of a questionnaire answered or dropped here.
    pub(super) fn forget_question_in_child(
        &mut self,
        req: &AskUserRequest,
        cx: &mut Context<Self>,
    ) {
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
            if let Some(at) = c.native.questions.iter().position(|q| q.rpc_id == id)
                && let Some(req) = c.native.questions.remove(at)
            {
                c.after_input(&req.tool_call_id, cx);
            }
        });
    }

    /// The agent process is up and the session is loaded, so session requests can go out.
    fn native_live(&self) -> bool {
        self.backend == Backend::Native
            && self.conn.is_some()
            && self.session_id.is_some()
            && !matches!(self.status, ThreadStatus::Connecting)
    }

    /// Renames the session, in arakun and in the agent's own session list (native
    /// Droid, Claude Code); before the session is loaded the rename waits for it.
    pub fn rename(&mut self, title: &str, cx: &mut Context<Self>) {
        let title = title.trim().to_string();
        if title.is_empty() {
            return;
        }
        let changed = title != self.meta.title;
        self.meta.title = title.clone();
        self.title_locked = true;
        if changed {
            cx.emit(ThreadEvent::TitleChanged);
            cx.notify();
        }
        if self.backend == Backend::Acp {
            if self.acp_live() {
                self.acp_send_title(title, cx);
            } else {
                self.native.want_title = Some(title);
            }
            return;
        }
        if !self.native_live() {
            self.native.want_title = Some(title);
            return;
        }
        self.native_send_title(title, cx);
    }

    fn native_send_title(&mut self, title: String, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let request = conn.request("droid.rename_session", json!({ "title": title }));
        cx.spawn(async move |this, cx| {
            if let Err(e) = request.await {
                let _ = this.update(cx, |t, cx| {
                    t.notice(format!("Renaming in Droid failed: {e}"), true, cx)
                });
            }
        })
        .detach();
    }

    /// Summarizes the conversation so far to free context (`droid.compact_session`).
    pub fn compact(&mut self, instructions: Option<String>, cx: &mut Context<Self>) {
        if self.remote {
            return self.remote_cannot("Compacting", cx);
        }
        if !self.native_live() && !self.acp_live() {
            if !self.native.deferred.contains(&Deferred::Compact) {
                self.native.deferred.push(Deferred::Compact);
            }
            return;
        }
        if self.backend == Backend::Acp {
            return self.acp_compact(instructions, cx);
        }
        let Some(conn) = self
            .conn
            .clone()
            .filter(|_| self.backend == Backend::Native)
        else {
            return;
        };
        if self.is_busy() {
            return;
        }
        let params = match instructions {
            Some(i) if !i.trim().is_empty() => json!({"customInstructions": i}),
            _ => json!({}),
        };
        self.set_status(ThreadStatus::Working, cx);
        self.status_detail = Some("Compacting the conversation…".into());
        self.native.compacting = true;
        let before = self.usage.context_percent();
        let request = conn.request("droid.compact_session", params);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| {
                t.status_detail = None;
                t.native.compacting = false;
                match result {
                    Ok(v) => {
                        if let Some(id) = str_of(&v, "newSessionId") {
                            t.session_id = Some(id);
                            t.meta.session_id = t.session_id.clone();
                        }
                        let removed = v["removedCount"].as_u64().unwrap_or(0);
                        t.push_entry(
                            Entry::Compacted {
                                removed,
                                before,
                                after: None,
                            },
                            cx,
                        );
                    }
                    Err(e) => t.notice(format!("Compacting failed: {e}"), true, cx),
                }
                t.set_status(ThreadStatus::Idle, cx);
                t.native_refresh_context(cx);
                t.meta_changed(cx);
                t.drain_queue(cx);
            });
        })
        .detach();
    }

    /// Copies the session into a new one (`droid.fork_session`); emits [`ThreadEvent::Forked`].
    pub fn fork(&mut self, cx: &mut Context<Self>) {
        if self.remote {
            return self.remote_cannot("Forking", cx);
        }
        if !self.native_live() && !self.acp_live() {
            if !self.native.deferred.contains(&Deferred::Fork) {
                self.native.deferred.push(Deferred::Fork);
            }
            return;
        }
        if self.backend == Backend::Acp {
            return self.acp_fork(cx);
        }
        let Some(conn) = self
            .conn
            .clone()
            .filter(|_| self.backend == Backend::Native)
        else {
            return;
        };
        let title = format!("{} (fork)", self.title());
        let request = conn.request("droid.fork_session", json!({ "title": title }));
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| match result {
                Ok(v) => {
                    if let Some(id) = str_of(&v, "newSessionId") {
                        let now = super::unix_now();
                        cx.emit(ThreadEvent::Forked(SessionMeta {
                            agent: t.kind,
                            session_id: Some(id),
                            title,
                            cwd: t.cwd.clone(),
                            created: now,
                            updated: now,
                            pr: None,
                        }));
                    }
                }
                Err(e) => t.notice(format!("Forking failed: {e}"), true, cx),
            });
        })
        .detach();
    }

    /// Whether the prompt at `ix` can be rewound to: live and idle, native Droid
    /// or an agent that forks sessions at a message (Claude Code).
    pub fn can_rewind(&self, ix: usize) -> bool {
        !self.remote
            && (self.native_live() || (self.acp_live() && self.caps.fork))
            && !self.is_busy()
            && matches!(
                self.entries.get(ix),
                Some(Entry::User {
                    message_id: Some(_),
                    ..
                })
            )
    }

    pub fn rewind_plan(&self) -> Option<&RewindPlan> {
        self.native.rewind.as_ref()
    }

    /// Asks Droid what going back to before the prompt at `ix` would undo;
    /// the plan waits in [`Self::rewind_plan`] for confirmation.
    pub fn begin_rewind(&mut self, ix: usize, cx: &mut Context<Self>) {
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        let Some(Entry::User {
            text,
            message_id: Some(message_id),
            ..
        }) = self.entries.get(ix)
        else {
            return;
        };
        if !self.can_rewind(ix) {
            return;
        }
        if self.backend == Backend::Acp {
            return self.acp_begin_rewind(ix, cx);
        }
        self.native.rewind = Some(RewindPlan {
            entry: ix,
            message_id: message_id.clone(),
            text: text.clone(),
            restore: Vec::new(),
            delete: Vec::new(),
            evicted: Vec::new(),
            loading: true,
            running: false,
        });
        cx.emit(ThreadEvent::RewindChanged { entry: ix });
        cx.notify();
        let request = conn.request(
            "droid.get_rewind_info",
            json!({"sessionId": session_id, "messageId": message_id}),
        );
        let message_id = message_id.clone();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| {
                let Some(plan) = t
                    .native
                    .rewind
                    .as_mut()
                    .filter(|p| p.message_id == message_id)
                else {
                    return;
                };
                match result {
                    Ok(v) => {
                        let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
                        plan.restore = list("availableFiles");
                        plan.delete = list("createdFiles");
                        plan.evicted = list("evictedFiles")
                            .iter()
                            .filter_map(|f| {
                                Some((
                                    str_of(f, "filePath")?,
                                    str_of(f, "reason").unwrap_or_default(),
                                ))
                            })
                            .collect();
                        plan.loading = false;
                    }
                    Err(e) => {
                        t.native.rewind = None;
                        t.notice(format!("Rewind is not available here: {e}"), true, cx);
                    }
                }
                cx.emit(ThreadEvent::RewindChanged { entry: ix });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cancel_rewind(&mut self, cx: &mut Context<Self>) {
        if let Some(plan) = self.native.rewind.take_if(|p| !p.running) {
            cx.emit(ThreadEvent::RewindChanged { entry: plan.entry });
            cx.notify();
        }
    }

    /// Runs the confirmed rewind: Droid restores the files and forks the
    /// session before the prompt; emits [`ThreadEvent::Rewound`].
    pub fn confirm_rewind(&mut self, cx: &mut Context<Self>) {
        if self.backend == Backend::Acp {
            return self.acp_confirm_rewind(cx);
        }
        let (Some(conn), Some(session_id)) = (self.conn.clone(), self.session_id.clone()) else {
            return;
        };
        let Some(plan) = self
            .native
            .rewind
            .as_mut()
            .filter(|p| !p.loading && !p.running)
        else {
            return;
        };
        plan.running = true;
        let entry = plan.entry;
        let title = format!("{} (rewind)", self.meta.title);
        let request = conn.request(
            "droid.execute_rewind",
            json!({
                "sessionId": session_id,
                "messageId": plan.message_id,
                "filesToRestore": plan.restore,
                "filesToDelete": plan.delete,
                "forkTitle": title,
            }),
        );
        cx.emit(ThreadEvent::RewindChanged { entry });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |t, cx| {
                let plan = t.native.rewind.take();
                cx.emit(ThreadEvent::RewindChanged { entry });
                cx.notify();
                let v = match result {
                    Ok(v) => v,
                    Err(e) => return t.notice(format!("Rewind failed: {e}"), true, cx),
                };
                let failed = v["failedRestoreCount"].as_u64().unwrap_or(0)
                    + v["failedDeleteCount"].as_u64().unwrap_or(0);
                if failed > 0 {
                    t.notice(
                        format!("Rewind could not put back {failed} file(s); check them by hand."),
                        true,
                        cx,
                    );
                }
                let Some(id) = str_of(&v, "newSessionId") else {
                    return t.notice("Rewind returned no session", true, cx);
                };
                let now = super::unix_now();
                cx.emit(ThreadEvent::Rewound {
                    meta: SessionMeta {
                        agent: t.kind,
                        session_id: Some(id),
                        title,
                        cwd: t.cwd.clone(),
                        created: now,
                        updated: now,
                        pr: None,
                    },
                    prompt: plan.map(|p| p.text).unwrap_or_default(),
                });
            });
        })
        .detach();
    }

    // ---- setup -------------------------------------------------------------

    pub(super) fn native_setup(&mut self, conn: Rc<Connection>, cx: &mut Context<Self>) {
        let cwd = self.cwd.display().to_string();
        let resume = self.session_id.clone();
        let have_history = self.suppress_replay;
        let mut init = json!({"machineId": machine_id(), "cwd": cwd});
        for (s, v) in &self.native.want {
            init[s.key()] = json!(v);
        }
        self._setup = Some(cx.spawn(async move |this, cx| {
            if let Some(id) = resume {
                let _ = this.update(cx, |t, cx| {
                    t.status_detail = Some("Loading the session…".into());
                    cx.notify();
                });
                match conn.request("droid.load_session", json!({"sessionId": id})).await {
                    Ok(v) => {
                        let _ = this.update(cx, |t, cx| t.native_ready(id, &v, !have_history, cx));
                        return;
                    }
                    Err(e) => {
                        super::log_note(&format!("droid.load_session failed: {e}"));
                        let _ = this.update(cx, |t, cx| {
                            t.notice(
                                "Droid could not reopen this session, so it starts fresh with no memory of the conversation above.",
                                false,
                                cx,
                            )
                        });
                    }
                }
            }
            match conn.request("droid.initialize_session", init).await {
                Ok(v) => match str_of(&v, "sessionId") {
                    Some(id) => {
                        let _ = this.update(cx, |t, cx| {
                            t.native.want.clear();
                            t.native_ready(id, &v, false, cx)
                        });
                    }
                    None => {
                        let _ = this.update(cx, |t, cx| {
                            t.fail(
                                FailedAt::Session(None),
                                "droid.initialize_session returned no sessionId".into(),
                                cx,
                            )
                        });
                    }
                },
                Err(e) => {
                    let _ = this.update(cx, |t, cx| {
                        let msg = t.describe_error(&e, "Starting the session failed");
                        t.fail(FailedAt::Session(Some(e.code)), msg, cx)
                    });
                }
            }
        }));
    }

    /// Transcripts saved before prompts carried Droid's message ids get them
    /// from the loaded history, matching prompts by text in order.
    fn adopt_message_ids(&mut self, history: &[Entry]) {
        let mut known = history.iter().filter_map(|e| match e {
            Entry::User {
                text,
                message_id: Some(id),
                ..
            } => Some((text, id)),
            _ => None,
        });
        for entry in &mut self.entries {
            let Entry::User {
                text, message_id, ..
            } = entry
            else {
                continue;
            };
            if message_id.is_some() {
                continue;
            }
            if let Some((_, id)) = known.by_ref().find(|(t, _)| *t == text) {
                *message_id = Some(id.clone());
            }
        }
    }

    /// `v` is the result of `initialize_session` or `load_session`.
    fn native_ready(&mut self, id: String, v: &Value, rebuild: bool, cx: &mut Context<Self>) {
        self.caps.load = true;
        self.caps.resume = true;
        self.agent_title = Some("Factory Droid (stream-jsonrpc)".into());
        if let Some(s) = NativeSettings::from_json(&v["settings"]) {
            self.native.settings = Some(s);
        }
        let models: Vec<ModelInfo> = v["availableModels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(ModelInfo::from_json)
            .collect();
        if !models.is_empty() {
            self.native.models = models;
        }
        self.apply_token_usage(v.get("inclusiveTokenUsage").or(v.get("tokenUsage")));
        let session = &v["session"];
        if let Some(title) = str_of(session, "title").filter(|t| !t.trim().is_empty()) {
            self.meta.title = title;
            self.title_locked = true;
            cx.emit(ThreadEvent::TitleChanged);
        }
        if let Some(messages) = session["messages"].as_array() {
            let entries = history(messages);
            if rebuild && !entries.is_empty() {
                self.set_entries(entries);
                cx.emit(ThreadEvent::Reset);
            } else {
                self.adopt_message_ids(&entries);
            }
        }
        // Settings chosen while connecting; a fresh session already got them in its init.
        let want = std::mem::take(&mut self.native.want);
        let pending: Vec<_> = want
            .into_iter()
            .filter(|(s, v)| {
                self.native
                    .settings
                    .as_ref()
                    .is_some_and(|cur| cur.get(*s) != v)
            })
            .collect();
        self.session_ready(id, &Value::Null, cx);
        for (s, v) in pending {
            self.set_setting(s, &v, cx);
        }
        if let Some(title) = self.native.want_title.take() {
            if title != self.meta.title {
                self.meta.title = title.clone();
                self.title_locked = true;
                cx.emit(ThreadEvent::TitleChanged);
            }
            self.native_send_title(title, cx);
        }
        for action in std::mem::take(&mut self.native.deferred) {
            match action {
                Deferred::Fork => self.fork(cx),
                Deferred::Compact => self.compact(None, cx),
            }
        }
        self.native_load_models(cx);
        self.native_refresh_context(cx);
    }

    /// The init result lists only enabled models; this also brings the disabled ones.
    fn native_load_models(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let request = conn.request("droid.list_models", json!({"includeDisabled": true}));
        cx.spawn(async move |this, cx| {
            if let Ok(v) = request.await {
                let models: Vec<ModelInfo> = v["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(ModelInfo::from_json)
                    .collect();
                if !models.is_empty() {
                    let _ = this.update(cx, |t, cx| {
                        t.native.models = models;
                        t.meta_changed(cx);
                    });
                }
            }
        })
        .detach();
    }

    fn native_refresh_settings(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let request = conn.request("droid.get_session_settings", json!({}));
        cx.spawn(async move |this, cx| {
            if let Ok(v) = request.await
                && let Some(s) = NativeSettings::from_json(v.get("settings").unwrap_or(&v))
            {
                let _ = this.update(cx, |t, cx| {
                    t.native.settings = Some(s);
                    t.meta_changed(cx);
                });
            }
        })
        .detach();
    }

    /// One `get_context_breakdown` call; made when a session opens and after each turn.
    pub(super) fn native_refresh_context(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let request = conn.request("droid.get_context_breakdown", json!({}));
        cx.spawn(async move |this, cx| {
            if let Ok(v) = request.await {
                let _ = this.update(cx, |t, cx| {
                    let meter = compaction_meter(&v);
                    t.usage.context_used = meter.map(|m| m.0);
                    t.usage.context_size = meter.map(|m| m.1);
                    t.fill_compacted_after(cx);
                    t.meta_changed(cx);
                });
            }
        })
        .detach();
    }

    fn apply_token_usage(&mut self, u: Option<&Value>) {
        let Some(u) = u.filter(|u| u.is_object()) else {
            return;
        };
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        self.usage.input_tokens = n("inputTokens");
        self.usage.output_tokens = n("outputTokens");
        self.usage.cache_write_tokens = n("cacheCreationTokens");
        self.usage.cache_read_tokens = n("cacheReadTokens");
        self.usage.thinking_tokens = n("thinkingTokens");
        if let Some(c) = u["factoryCredits"].as_u64() {
            self.usage.credits = Some(c);
        }
    }

    // ---- turns -------------------------------------------------------------

    pub(super) fn native_send(
        &mut self,
        conn: &Connection,
        text: String,
        message_id: String,
        images: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.native.turn_error = false;
        let mut params = json!({"text": text, "messageId": message_id});
        let mut paths = Vec::new();
        for path in images {
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if len > attach::MAX_IMAGE_BYTES {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.notice(
                    format!("Not attached: {name} is larger than 5 MB"),
                    false,
                    cx,
                );
            } else {
                paths.push(path.display().to_string());
            }
        }
        if !paths.is_empty() {
            params["imagePaths"] = json!(paths);
        }
        let request = conn.request("droid.add_user_message", params);
        // The reply only acknowledges the message; `agent_turn_completed` ends the turn.
        self._prompt = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = request.await {
                let _ = this.update(cx, |t, cx| t.finish_turn(Err(e), cx));
            }
        }));
    }

    pub(super) fn native_cancel(&mut self, conn: &Connection, cx: &mut Context<Self>) {
        let request = conn.request("droid.interrupt_session", json!({}));
        cx.spawn(async move |this, cx| {
            if let Err(e) = request.await
                && e.code != RpcError::DISCONNECTED
            {
                let _ = this.update(cx, |t, cx| {
                    t.notice(format!("Stopping failed: {e}"), true, cx)
                });
            }
        })
        .detach();
    }

    /// Answers the questionnaires nobody answered as cancelled. `keep_running`:
    /// those of subagents still running stay, as their permission requests do.
    pub(super) fn drop_questions(&mut self, keep_running: bool, cx: &mut Context<Self>) {
        self.drop_questions_where(
            |t, q| {
                !(keep_running
                    && q.session
                        .as_deref()
                        .and_then(|s| t.subagent(s))
                        .is_some_and(|s| s.state.is_running()))
            },
            cx,
        );
    }

    /// Answers the questionnaires `gone` picks as cancelled, and drops the
    /// copies their subagents' threads hold.
    pub(super) fn drop_questions_where(
        &mut self,
        gone: impl Fn(&Thread, &AskUserRequest) -> bool,
        cx: &mut Context<Self>,
    ) {
        let asks = std::mem::take(&mut self.native.questions);
        let (drop, keep): (Vec<_>, Vec<_>) = asks.into_iter().partition(|q| gone(self, q));
        self.native.questions = keep.into();
        for req in drop {
            if let Some(conn) = &self.conn {
                let reply = if req.fields.is_empty() {
                    json!({"answers": [], "cancelled": true})
                } else {
                    json!({"action": "cancel"})
                };
                conn.respond(req.rpc_id.clone(), reply);
            }
            self.forget_question_in_child(&req, cx);
            // Not `after_input`: callers settle the status, and a turn ending
            // here must not start the next queued prompt under itself.
            if let Some(&ix) = self.tool_index.get(&req.tool_call_id) {
                self.entries_changed(ix, cx);
            }
        }
    }

    fn native_turn_completed(&mut self, n: &Value, cx: &mut Context<Self>) {
        self.apply_token_usage(n.get("cumulativeTokenUsage"));
        let reason = str_of(n, "reason").unwrap_or_else(|| "completed".into());
        if self.turn_started.is_none() && !self.is_busy() {
            self.meta_changed(cx);
            return;
        }
        self.drop_questions(false, cx);
        let error = std::mem::take(&mut self.native.turn_error);
        let stop = match reason.as_str() {
            "completed" | "spec_handoff" => "end_turn",
            "cancelled" | "permission_rejected" => "cancelled",
            _ => "error",
        };
        self.cancelling = false;
        self.settle_turn(cx);
        let plan_declined = reason == "permission_rejected"
            && self.entries.iter().rev().find_map(|e| match e {
                Entry::Tool(t) => Some(t.kind == "switch_mode"),
                _ => None,
            }) == Some(true);
        if plan_declined {
            self.notice(
                "Still in spec mode. Say what to change and Droid revises the plan.",
                false,
                cx,
            );
        } else if let Some((text, is_error)) = turn_notice(&reason)
            && !(is_error && error)
        {
            self.notice(text, is_error, cx);
        }
        self.set_status(ThreadStatus::Idle, cx);
        self._prompt = None;
        cx.emit(ThreadEvent::Finished {
            stop_reason: stop.into(),
        });
        self.native_refresh_context(cx);
        self.meta_changed(cx);
        self.drain_queue(cx);
    }

    // ---- incoming ------------------------------------------------------------

    pub(super) fn native_notification(
        &mut self,
        method: &str,
        params: &Value,
        cx: &mut Context<Self>,
    ) {
        if method != "droid.session_notification" {
            return;
        }
        let n = &params["notification"];
        match n["type"].as_str().unwrap_or("") {
            "settings_updated" => {
                if let Some(s) = NativeSettings::from_json(&n["settings"])
                    && self.native.settings.as_ref() != Some(&s)
                {
                    self.native.settings = Some(s);
                    self.meta_changed(cx);
                }
            }
            "droid_working_state_changed" => {
                let state = str_of(n, "newState").unwrap_or_default();
                const COMPACTING: &str = "compacting_conversation";
                if state == COMPACTING && self.native.working != COMPACTING {
                    self.native.compact_before = self.usage.context_percent();
                } else if self.native.working == COMPACTING
                    && state != COMPACTING
                    && !self.native.compacting
                {
                    let before = self.native.compact_before.take();
                    self.push_entry(
                        Entry::Compacted {
                            removed: 0,
                            before,
                            after: None,
                        },
                        cx,
                    );
                    self.native_refresh_context(cx);
                }
                self.native.working = state;
            }
            "llm_retry" => {
                let attempt = n["attempt"].as_u64().unwrap_or(1);
                let what = match n["reason"].as_str().unwrap_or("") {
                    "overloaded" => "Model overloaded",
                    "rate_limited" => "Rate limited",
                    "network" => "Connection error",
                    "empty_response" => "Empty response",
                    _ => "API error",
                };
                if self.is_busy() {
                    self.set_retry(Some(format!("{what} · retrying (attempt {attempt})…")), cx);
                }
            }
            "assistant_text_delta" => self.native_delta(false, n, cx),
            "thinking_text_delta" => self.native_delta(true, n, cx),
            "create_message" => self.native_message(&n["message"], cx),
            "assistant_message_retracted" => {
                if let Some(id) = str_of(n, "messageId") {
                    let ixs: Vec<usize> = self
                        .native
                        .blocks
                        .iter()
                        .filter(|((m, _), _)| *m == id)
                        .map(|(_, &ix)| ix)
                        .collect();
                    for ix in ixs {
                        if let Some(Entry::Agent { text } | Entry::Thought { text }) =
                            self.entries.get_mut(ix)
                        {
                            text.clear();
                            self.entries_changed(ix, cx);
                        }
                    }
                }
            }
            "tool_call" => {
                let tool_use = &n["toolUse"];
                self.native_tool_use(tool_use, cx);
            }
            "tool_execution_phase_changed" => {
                let id = str_of(n, "toolUseId").unwrap_or_default();
                if let Some(&ix) = self.tool_index.get(&id)
                    && let Some(Entry::Tool(t)) = self.entries.get_mut(ix)
                    && matches!(t.status, ToolStatus::Pending | ToolStatus::InProgress)
                {
                    t.status = match n["phase"].as_str() {
                        Some("queued") => ToolStatus::Pending,
                        _ => ToolStatus::InProgress,
                    };
                    self.entries_changed(ix, cx);
                }
            }
            "tool_result" => {
                let id = str_of(n, "toolUseId").unwrap_or_default();
                if let Some(&ix) = self.tool_index.get(&id)
                    && let Some(Entry::Tool(t)) = self.entries.get_mut(ix)
                {
                    finish_tool(t, &n["content"], n["isError"].as_bool().unwrap_or(false));
                    self.fill_replaced(ix);
                    self.entries_changed(ix, cx);
                }
            }
            "permission_resolved" => {
                let id = str_of(n, "requestId");
                let before = self.permissions.len();
                self.permissions
                    .retain(|p| p.rpc_id.as_str().map(str::to_string) != id);
                if self.permissions.len() != before {
                    if self.permissions.is_empty()
                        && self.native.questions.is_empty()
                        && self.is_busy()
                    {
                        self.set_status(ThreadStatus::Working, cx);
                    }
                    self.meta_changed(cx);
                }
            }
            "session_token_usage_changed" => {
                self.apply_token_usage(n.get("inclusiveTokenUsage").or(n.get("tokenUsage")));
                self.meta_changed(cx);
            }
            "session_title_updated" => {
                if let Some(title) = str_of(n, "title").filter(|t| !t.trim().is_empty())
                    && title != self.meta.title
                {
                    self.meta.title = title;
                    self.title_locked = true;
                    cx.emit(ThreadEvent::TitleChanged);
                    cx.notify();
                }
            }
            "agent_turn_completed" => self.native_turn_completed(n, cx),
            "error" => {
                let msg = str_of(n, "message").unwrap_or_else(|| "Droid reported an error.".into());
                self.native.turn_error = true;
                self.notice(msg, true, cx);
            }
            _ => {}
        }
    }

    fn native_delta(&mut self, thought: bool, n: &Value, cx: &mut Context<Self>) {
        let delta = str_of(n, "textDelta").unwrap_or_default();
        if delta.is_empty() {
            return;
        }
        self.set_retry(None, cx);
        let key = (
            str_of(n, "messageId").unwrap_or_default(),
            n["blockIndex"].as_u64().unwrap_or(0),
        );
        if let Some(&ix) = self.native.blocks.get(&key)
            && let Some(Entry::Agent { text } | Entry::Thought { text }) = self.entries.get_mut(ix)
        {
            text.push_str(&delta);
            self.entries_changed(ix, cx);
            return;
        }
        let entry = if thought {
            Entry::Thought { text: delta }
        } else {
            Entry::Agent { text: delta }
        };
        let ix = self.push_entry(entry, cx);
        self.native.blocks.insert(key, ix);
    }

    /// A finished message. User messages are ours (already shown); assistant
    /// ones settle the streamed text and carry the tool calls.
    fn native_message(&mut self, m: &Value, cx: &mut Context<Self>) {
        if m["role"].as_str() != Some("assistant") || m["visibility"].as_str() == Some("llm_only") {
            return;
        }
        let id = str_of(m, "id").unwrap_or_default();
        for (i, b) in m["content"].as_array().into_iter().flatten().enumerate() {
            let thought = match b["type"].as_str() {
                Some("text") => false,
                Some("thinking") => true,
                Some("tool_use") => {
                    self.native_tool_use(b, cx);
                    continue;
                }
                _ => continue,
            };
            let full = str_of(b, if thought { "thinking" } else { "text" }).unwrap_or_default();
            let key = (id.clone(), i as u64);
            match self.native.blocks.get(&key) {
                Some(&ix) => {
                    if let Some(Entry::Agent { text } | Entry::Thought { text }) =
                        self.entries.get_mut(ix)
                        && *text != full
                    {
                        *text = full;
                        self.entries_changed(ix, cx);
                    }
                }
                None if !full.trim().is_empty() => {
                    let ix = self.push_entry(
                        if thought {
                            Entry::Thought { text: full }
                        } else {
                            Entry::Agent { text: full }
                        },
                        cx,
                    );
                    self.native.blocks.insert(key, ix);
                }
                None => {}
            }
        }
    }

    fn native_tool_use(&mut self, tool_use: &Value, cx: &mut Context<Self>) {
        self.set_retry(None, cx);
        let Some(id) = str_of(tool_use, "id") else {
            return;
        };
        let name = str_of(tool_use, "name").unwrap_or_default();
        let input = tool_use.get("input").cloned().unwrap_or(Value::Null);
        if name == "TodoWrite" {
            let plan = parse_todos(&input);
            if !plan.is_empty() {
                self.set_plan(plan, cx);
            }
            return;
        }
        let ix = match self.tool_index.get(&id) {
            Some(&ix) => ix,
            None => self.push_entry(Entry::Tool(new_tool(&id)), cx),
        };
        if let Some(Entry::Tool(t)) = self.entries.get_mut(ix) {
            // Input streams in; an empty object would wipe what an earlier event had.
            if input.as_object().is_some_and(|o| !o.is_empty()) || t.raw_input.is_none() {
                describe_tool(t, &name, &input);
            }
            self.fill_replaced(ix);
            self.entries_changed(ix, cx);
        }
    }

    pub(super) fn native_request(
        &mut self,
        id: Value,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        match method {
            "droid.request_permission" => {
                let uses = params["toolUses"].as_array().cloned().unwrap_or_default();
                let first = uses.first().cloned().unwrap_or(Value::Null);
                let tool_use = &first["toolUse"];
                let tool_call_id = str_of(tool_use, "id").unwrap_or_default();
                if !tool_call_id.is_empty() && !self.tool_index.contains_key(&tool_call_id) {
                    self.native_tool_use(tool_use, cx);
                }
                let details = &first["details"];
                let confirmation = str_of(&first, "confirmationType").unwrap_or_default();
                // The edit details carry the whole file before and after: a better preview.
                if confirmation == "edit"
                    && let (Some(path), Some(new)) =
                        (str_of(details, "filePath"), str_of(details, "newContent"))
                    && let Some(&ix) = self.tool_index.get(&tool_call_id)
                    && let Some(Entry::Tool(t)) = self.entries.get_mut(ix)
                {
                    t.content = vec![ToolContent::Diff {
                        path: path.into(),
                        old_text: str_of(details, "oldContent"),
                        new_text: new,
                    }];
                    recount(t);
                }
                let plan = (confirmation == "exit_spec_mode").then(|| SpecPlan {
                    title: str_of(details, "title").unwrap_or_default(),
                    plan: str_of(details, "plan").unwrap_or_default(),
                });
                let mut options: Vec<PermissionOption> = params["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| {
                        let value = str_of(o, "value")?;
                        // Handing off to a new session would leave this thread behind.
                        if value.starts_with("proceed_new_session") {
                            return None;
                        }
                        let kind = match value.as_str() {
                            "cancel" => "reject_once",
                            "proceed_once" | "proceed_edit" => "allow_once",
                            _ => "allow_always",
                        };
                        let mut name = str_of(o, "label").unwrap_or_else(|| value.clone());
                        if plan.is_some() {
                            match value.as_str() {
                                "proceed_once" => name = "Approve".into(),
                                "proceed_auto_run_low" => name = "Approve, Auto Low".into(),
                                "proceed_auto_run_medium" => name = "Approve, Auto Medium".into(),
                                "proceed_auto_run_high" => name = "Approve, Auto High".into(),
                                "cancel" => name = "Keep planning".into(),
                                _ => {}
                            }
                        }
                        Some(PermissionOption {
                            id: value,
                            name,
                            kind: kind.into(),
                        })
                    })
                    .collect();
                if options.is_empty() {
                    options = vec![
                        PermissionOption {
                            id: "proceed_once".into(),
                            name: "Yes, allow".into(),
                            kind: "allow_once".into(),
                        },
                        PermissionOption {
                            id: "cancel".into(),
                            name: "No".into(),
                            kind: "reject_once".into(),
                        },
                    ];
                }
                let title = self
                    .tool_index
                    .get(&tool_call_id)
                    .and_then(|&i| match &self.entries[i] {
                        Entry::Tool(t) => Some(t.title.clone()),
                        _ => None,
                    })
                    .or_else(|| str_of(details, "fullCommand"))
                    .unwrap_or_else(|| "Run a tool".into());
                // A spec-mode exit names its plan; the tool entry may be missing.
                let title = match &plan {
                    Some(p) if !title.starts_with("Plan") => {
                        let name = if !p.title.trim().is_empty() {
                            p.title.trim().to_string()
                        } else {
                            p.plan
                                .lines()
                                .map(str::trim)
                                .find(|l| !l.is_empty())
                                .unwrap_or_default()
                                .to_string()
                        };
                        if name.is_empty() {
                            title
                        } else {
                            format!("Plan: {name}")
                        }
                    }
                    _ => title,
                };
                let title = if uses.len() > 1 {
                    format!("{title} (+{} more)", uses.len() - 1)
                } else {
                    title
                };
                self.permissions.push_back(PermissionRequest {
                    rpc_id: id,
                    tool_call_id: tool_call_id.clone(),
                    title,
                    options,
                    plan,
                    session: None,
                });
                if let Some(&ix) = self.tool_index.get(&tool_call_id) {
                    self.entries_changed(ix, cx);
                }
                self.set_status(ThreadStatus::NeedsInput, cx);
                cx.emit(ThreadEvent::NeedsPermission);
                cx.notify();
            }
            "droid.ask_user" => {
                let questions: Vec<Question> = params["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(i, q)| Question {
                        index: q["index"].as_u64().unwrap_or(i as u64 + 1),
                        topic: str_of(q, "topic").unwrap_or_default(),
                        question: str_of(q, "question").unwrap_or_default(),
                        options: q["options"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        multi: q["multiSelect"].as_bool().unwrap_or(false),
                    })
                    .collect();
                let tool_call_id = str_of(&params, "toolCallId").unwrap_or_default();
                self.native.questions.push_back(AskUserRequest {
                    rpc_id: id,
                    tool_call_id: tool_call_id.clone(),
                    questions,
                    fields: Vec::new(),
                    session: None,
                });
                if let Some(&ix) = self.tool_index.get(&tool_call_id) {
                    self.entries_changed(ix, cx);
                }
                self.set_status(ThreadStatus::NeedsInput, cx);
                cx.emit(ThreadEvent::NeedsPermission);
                cx.notify();
            }
            _ => conn.respond_error(id, -32601, &format!("method not found: {method}")),
        }
    }

    /// After a permission or question is answered: back to work if nothing else waits.
    pub(super) fn after_input(&mut self, tool_call_id: &str, cx: &mut Context<Self>) {
        if let Some(&ix) = self.tool_index.get(tool_call_id) {
            self.entries_changed(ix, cx);
        }
        if self.permissions.is_empty()
            && self.native.questions.is_empty()
            && self.status == ThreadStatus::NeedsInput
        {
            // A subagent asking after the turn was handed back leaves it idle.
            let status = if self.parent.is_some() {
                ThreadStatus::Working
            } else {
                self.resting_status()
            };
            self.set_status(status, cx);
            if self.status == ThreadStatus::Idle {
                self.drain_queue(cx);
            }
        }
        self.meta_changed(cx);
    }
}

/// `0.04` → "0.04", `2.0` → "2", `1.6` → "1.6".
fn fmt_multiplier(x: f64) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// What Droid's own compaction meter counts, as (used, limit) tokens, from a
/// `get_context_breakdown` result. `get_context_stats` is no use here: it sums
/// every message in the session file, and compacting keeps the old ones there,
/// so its count only grows. The breakdown's `usedTokens` estimate also goes
/// wrong after some compactions; the provider-reported tokens of the last call
/// do not, and Droid resets them to the summary's size when it compacts.
fn compaction_meter(v: &Value) -> Option<(u64, u64)> {
    /// Droid's `systemPromptTokens`, left out of both sides of its meter.
    const SYSTEM_PROMPT_TOKENS: u64 = 11_000;
    let last = v["lastCallCompactionTokens"].as_u64().filter(|&n| n > 0)?;
    let budget = v["contextBudget"].as_u64()?;
    let limit = budget.saturating_sub(SYSTEM_PROMPT_TOKENS).max(1);
    Some((last.saturating_sub(SYSTEM_PROMPT_TOKENS).min(limit), limit))
}

/// Compact counts for the usage line: 950, 12.3k, 4.1M.
pub fn fmt_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => trim_one(n as f64 / 1_000.0, "k"),
        _ => trim_one(n as f64 / 1_000_000.0, "M"),
    }
}

fn trim_one(x: f64, unit: &str) -> String {
    let s = if x >= 100.0 {
        format!("{x:.0}")
    } else {
        format!("{x:.1}")
    };
    format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
}

#[cfg(test)]
mod tests {
    use super::super::Usage;
    use super::*;

    #[test]
    fn claude_question_form_round_trips() {
        // As claude-agent-acp 0.90 asks two AskUserQuestion questions.
        let params = json!({
            "mode": "form", "sessionId": "s", "toolCallId": "t1",
            "message": "Please answer the following questions.",
            "requestedSchema": {"type": "object", "properties": {
                "question_0": {"type": "string", "title": "DB", "description": "Which database?",
                    "oneOf": [{"const": "Postgres", "title": "Postgres"}, {"const": "SQLite", "title": "SQLite"}]},
                "question_0_custom": {"type": "string", "title": "Other"},
                "question_1": {"type": "array", "title": "Extras", "description": "Add what?",
                    "items": {"anyOf": [{"const": "Auth", "title": "Auth"}, {"const": "Cache", "title": "Cache"}]}},
                "question_1_custom": {"type": "string", "title": "Other"}
            }}
        });
        let req = AskUserRequest::from_form(json!(7), &params).unwrap();
        assert_eq!(req.tool_call_id, "t1");
        assert_eq!(req.questions.len(), 2);
        assert_eq!(req.questions[0].question, "Which database?");
        assert_eq!(req.questions[0].options, ["Postgres", "SQLite"]);
        assert!(req.questions[1].multi);
        let content = form_content(
            &req,
            &["SQLite, only in tests".into(), "Auth, Cache, logs".into()],
        );
        assert_eq!(
            content,
            json!({"question_0": "SQLite", "question_0_custom": "only in tests",
                   "question_1": ["Auth", "Cache"], "question_1_custom": "logs"})
        );
        let content = form_content(&req, &["MySQL".into(), "Cache".into()]);
        assert_eq!(
            content,
            json!({"question_0_custom": "MySQL", "question_1": ["Cache"]})
        );
        assert!(AskUserRequest::from_form(json!(8), &json!({"mode": "url", "url": "x"})).is_none());
    }

    fn percent(v: Value) -> Option<u64> {
        let (used, size) = compaction_meter(&v)?;
        Usage {
            context_used: Some(used),
            context_size: Some(size),
            ..Default::default()
        }
        .context_percent()
    }

    #[test]
    fn compaction_meter_matches_droid() {
        let v = json!({"contextBudget": 250000, "lastCallCompactionTokens": 224863, "usedTokens": 659449});
        assert_eq!(percent(v), Some(89));
        let v = json!({"contextBudget": 250000, "lastCallCompactionTokens": 33564});
        assert_eq!(percent(v), Some(9));
    }

    #[test]
    fn compaction_meter_drops_after_compacting() {
        let before = json!({"contextBudget": 133424, "lastCallCompactionTokens": 19051});
        let after = json!({"contextBudget": 133424, "lastCallCompactionTokens": 570});
        assert_eq!(percent(before), Some(7));
        assert_eq!(percent(after), Some(0));
    }

    #[test]
    fn compaction_meter_caps_and_waits_for_a_call() {
        let over = json!({"contextBudget": 100000, "lastCallCompactionTokens": 150000});
        assert_eq!(percent(over), Some(100));
        assert_eq!(
            compaction_meter(&json!({"contextBudget": 133424, "usedTokens": 10142})),
            None
        );
    }
}
