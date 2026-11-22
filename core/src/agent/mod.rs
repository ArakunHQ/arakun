//! Coding agents (Claude Code, Droid) driven over JSON-RPC 2.0 on the agent's
//! stdio: ACP, the Agent Client Protocol, for Claude, and Factory's native
//! `stream-jsonrpc` for Droid (`ARAKUN_DROID_ACP=1` puts Droid on ACP as well).
//! [`Thread`] owns one agent process and one session; [`ThreadView`] renders its
//! transcript.

pub(crate) mod anthropic;
pub mod attach;
pub mod catalog;
pub mod chart;
pub mod claude_auth;
pub mod claude_ext;
pub mod cli_login;
pub mod cliproxy;
pub mod devin_plan;
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
pub mod sandbox;
pub mod saved;
mod search;
pub mod seats;
mod selection;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod session_ssh;
mod shell_diff;
pub mod skills_sh;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod ssh_request;
mod thread;
mod view;
mod web;
pub mod workers_ai;

pub use kind::AgentKind;
pub use launch::{
    adapter_install, agy_bin, antigravity_env_key, antigravity_signed_in, child_path_env,
    claude_cli, claude_code_binary, codex_cli, codex_env_key, codex_sign_in, cursor_bin,
    cursor_env_key, cursor_signed_in, devin_bin, devin_env_key, devin_signed_in, droid_bin,
    install_adapter, login_command, logout_command, opencode_all_models, opencode_bin,
    opencode_go_login_command, opencode_models, opencode_providers, which,
};
pub use markdown::{copy_with_feedback, was_copied};
pub use selection::{TextMap, mark_color};
pub use shell_diff::ShellChange;
pub use thread::{
    AskUserRequest, Backend, CheckAnswer, Choice, ConfigOption, Entry, FailedAt, ModelInfo,
    NativeSettings, NoticeMark, NoticePlace, PermissionOption, PermissionRequest, PlanEntry,
    PlanLimit, PlanStatus, Question, RemoteAction, Restored, RewindPlan, SessionMeta, Setting,
    SettingOption, SpecPlan, Subagent, SubagentRecord, SubagentState, Thread, ThreadEvent,
    ThreadStatus, ToolCall, ToolContent, ToolStatus, Transcript, Usage, acp_label, acp_option,
    acp_setting_options, apply_setting, edited_files, fmt_cost, fmt_count, setting_label,
    setting_options, staged_notice,
};
pub use view::{ThreadView, ThreadViewEvent};
