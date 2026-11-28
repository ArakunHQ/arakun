//! The "on behalf of" block that tells Linear Reviews an agent wrote a pull
//! request, review or comment. A delegated session posts with the person's
//! own GitHub account; without the block it reads as written by hand.
//!
//! The block is an HTML comment, so GitHub does not show it:
//!
//! ```text
//! <!-- linear:extension {"version":1,"plugins":[{"plugin":"onBehalfOf","agent":"claude","model":"claude-opus-5-5"}]} -->
//! ```

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::agent::AgentKind;

/// How every block starts. A body that holds it already has one, whoever
/// wrote it, and gets no second.
const MARK: &str = "<!-- linear:extension";

/// The most characters of a model name in the block.
const MODEL_LIMIT: usize = 100;

/// Linear's name for `agent`, for the agents Linear Reviews knows. Other
/// agents get no block.
fn agent_id(agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::Claude => Some("claude"),
        AgentKind::Codex => Some("codex"),
        AgentKind::OpenCode => Some("opencode"),
        _ => None,
    }
}

/// A model name as the agent reported it, when it is safe in an HTML
/// comment and reads as a name. `-->` or a line break would end the block.
fn clean_model(model: &str) -> Option<&str> {
    let model = model.trim();
    let ok = !model.is_empty()
        && model.len() <= MODEL_LIMIT
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@[]+-".contains(c))
        && !model.contains("--");
    ok.then_some(model)
}

/// The block for a session of `agent` on `model`, or None for an agent
/// Linear Reviews does not know.
pub fn block(agent: AgentKind, model: Option<&str>) -> Option<String> {
    let mut plugin = json!({ "plugin": "onBehalfOf", "agent": agent_id(agent)? });
    if let Some(model) = model.and_then(clean_model) {
        plugin["model"] = json!(model);
    }
    let ext = json!({ "version": 1, "plugins": [plugin] });
    Some(format!("{MARK} {ext} -->"))
}

/// `body` with `block` at its end, or None when it has a block already. The
/// rest of the body stays as it was.
pub fn with_block(body: &str, block: &str) -> Option<String> {
    if body.contains(MARK) {
        return None;
    }
    let body = body.trim_end();
    Some(if body.is_empty() {
        block.to_string()
    } else {
        format!("{body}\n\n{block}")
    })
}

/// `owner/repo` and the number of a GitHub pull request's address.
fn pr_of(url: &str) -> Option<(String, u64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo, pulls, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let name = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if pulls != "pull" || !name(owner) || !name(repo) {
        return None;
    }
    let number = number.split(['#', '?']).next()?.parse().ok()?;
    Some((format!("{owner}/{repo}"), number))
}

/// Adds `block` to the description of the pull request at `url`, once.
/// False when it had one already. Blocks on `gh`; run it off the UI thread.
/// Goes through `gh api`: `gh pr edit` fails on some versions of gh.
pub fn mark_pr(dir: &Path, url: &str, block: &str) -> Result<bool> {
    if crate::host::github_read_only() {
        bail!("{}", crate::host::get().github_read_only);
    }
    let (repo, number) = pr_of(url).ok_or_else(|| anyhow!("not a GitHub pull request"))?;
    let path = format!("repos/{repo}/pulls/{number}");
    let out = crate::git::output(
        crate::git::gh(dir).args(["api", &path, "--jq", ".body // \"\""]),
        crate::platform::GIT_REMOTE,
    )?;
    if !out.status.success() {
        bail!(
            "gh api failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // `--jq` prints a string with a line break after it.
    let body = String::from_utf8_lossy(&out.stdout);
    let body = body.strip_suffix('\n').unwrap_or(&body);
    let Some(body) = with_block(body, block) else {
        return Ok(false);
    };
    let input = serde_json::to_vec(&json!({ "body": body }))?;
    let out = crate::platform::output_within(
        crate::git::gh(dir).args(["api", "-X", "PATCH", &path, "--input", "-"]),
        Some(&input),
        crate::platform::GIT_REMOTE,
    )?;
    if !out.status.success() {
        bail!(
            "gh api failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_names_the_agent_and_its_model() {
        assert_eq!(
            block(AgentKind::Claude, Some("claude-opus-5-5")).as_deref(),
            Some(
                r#"<!-- linear:extension {"version":1,"plugins":[{"plugin":"onBehalfOf","agent":"claude","model":"claude-opus-5-5"}]} -->"#
            )
        );
        assert_eq!(
            block(AgentKind::Codex, None).as_deref(),
            Some(
                r#"<!-- linear:extension {"version":1,"plugins":[{"plugin":"onBehalfOf","agent":"codex"}]} -->"#
            )
        );
        assert!(
            block(AgentKind::OpenCode, None)
                .unwrap()
                .contains("\"opencode\"")
        );
    }

    #[test]
    fn agents_linear_does_not_know_get_no_block() {
        assert_eq!(block(AgentKind::Droid, Some("x")), None);
        assert_eq!(block(AgentKind::WorkersAi, None), None);
    }

    #[test]
    fn a_model_that_could_end_the_comment_is_left_out() {
        for model in ["x --> <b>", "a\nb", "", "a--b", &"m".repeat(101)] {
            let b = block(AgentKind::Claude, Some(model)).unwrap();
            assert!(!b.contains("model"), "{model:?}: {b}");
        }
        let b = block(AgentKind::OpenCode, Some("openrouter/qwen3-coder:free")).unwrap();
        assert!(b.contains(r#""model":"openrouter/qwen3-coder:free""#));
    }

    #[test]
    fn the_block_goes_once_and_keeps_the_body() {
        let b = block(AgentKind::Claude, None).unwrap();
        let body = with_block("## Changes\n\nIt works.\n\n", &b).unwrap();
        assert_eq!(body, format!("## Changes\n\nIt works.\n\n{b}"));
        assert_eq!(with_block(&body, &b), None);
        assert_eq!(with_block("", &b).as_deref(), Some(b.as_str()));
        // One the agent wrote itself counts too.
        let other = block(AgentKind::Codex, None).unwrap();
        assert_eq!(with_block(&format!("x\n{other}"), &b), None);
    }

    #[test]
    fn only_a_github_pull_request_is_marked() {
        assert_eq!(
            pr_of("https://github.com/ArakunHQ/arakun/pull/1422"),
            Some(("ArakunHQ/arakun".into(), 1422))
        );
        assert_eq!(
            pr_of("https://github.com/a/b/pull/7#issuecomment-1"),
            Some(("a/b".into(), 7))
        );
        for url in [
            "https://github.com/a/b/issues/7",
            "https://gitlab.com/a/b/pull/7",
            "https://github.com/../b/pull/7",
            "https://github.com/a/b?x/pull/7",
            "https://github.com/a/b/pull/x",
        ] {
            assert_eq!(pr_of(url), None, "{url}");
        }
    }
}
