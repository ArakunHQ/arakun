//! Coding agents (Claude Code, Droid) driven over JSON-RPC 2.0 on the agent's
//! stdio: ACP, the Agent Client Protocol, for Claude, and Factory's native
//! `stream-jsonrpc` for Droid (`ARAKUN_DROID_ACP=1` puts Droid on ACP as well).
//! [`Thread`] owns one agent process and one session; [`ThreadView`] renders its
//! transcript.
//!
//! Each binary that includes this module uses a different slice of it (the app,
//! `acp_probe`, `thread_demo`), so unused-item lints would only be noise here.
#![allow(dead_code)]

pub(crate) mod anthropic;
pub mod attach;
pub mod catalog;
pub mod chart;
pub mod claude_auth;
pub mod claude_ext;
pub mod cli_login;
pub mod cliproxy;
pub mod diff;
pub mod extensions;
mod kind;
mod knobs;
mod launch;
pub mod ledger;
pub mod log;
mod loops;
pub mod markdown;
pub mod marketplace;
pub mod notes;
pub mod opencode_plan;
pub mod plan;
pub mod plugins;
pub mod publishers;
mod rpc;
pub mod saved;
mod search;
mod selection;
mod shell_diff;
mod thread;
mod view;
mod web;
pub mod workers_ai;

pub use kind::AgentKind;
// The palette uses it; the examples that include this module leave the palette out.
#[allow(unused_imports)]
pub use launch::claude_code_binary;
#[allow(unused_imports)]
pub use launch::{
    adapter_install, child_path_env, claude_cli, codex_cli, codex_env_key, codex_sign_in,
    droid_bin, install_codex, login_command, logout_command, opencode_all_models, opencode_bin,
    opencode_go_login_command, opencode_models, opencode_providers, which,
};
#[allow(unused_imports)]
pub use markdown::{copy_with_feedback, was_copied};
#[allow(unused_imports)]
pub use selection::{TextMap, mark_color};
#[allow(unused_imports)]
pub use shell_diff::ShellChange;
#[allow(unused_imports)]
pub use thread::{
    AskUserRequest, Backend, CheckAnswer, Choice, ConfigOption, Entry, FailedAt, ModelInfo,
    NativeSettings, NoticeMark, NoticePlace, PermissionOption, PermissionRequest, PlanEntry,
    PlanLimit, Question, RemoteAction, Restored, RewindPlan, SessionMeta, Setting, SettingOption,
    SpecPlan, Subagent, SubagentRecord, SubagentState, Thread, ThreadEvent, ThreadStatus, ToolCall,
    ToolContent, ToolStatus, Transcript, Usage, acp_label, acp_option, acp_setting_options,
    apply_setting, fmt_cost, fmt_count, setting_label, setting_options,
};
#[allow(unused_imports)]
pub use view::{ThreadView, ThreadViewEvent};
