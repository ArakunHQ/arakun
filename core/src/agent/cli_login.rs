//! The agent CLIs' own sign-ins. Arakun has no account of its own for an
//! agent: it runs the CLI, which signs in with what it already has, and the
//! plan limits are read with that same sign-in from the CLI's files. That is
//! why Arakun never asks to sign in or to approve access, and the notices here
//! say so where people see it.
//!
//! The same rules hold for every provider:
//!
//! - Read only. The CLI owns the file, writes it and renews it. A renewal by
//!   Arakun could rotate the refresh token and sign the CLI out.
//! - Sent only to the provider's own host, over https ([`check`]). Never to
//!   `ANTHROPIC_BASE_URL` or another gateway, even when the CLI runs on one.
//! - Never saved, logged or shown. curl gets it in a config on stdin.

use anyhow::{Result, bail};

use super::AgentKind;

/// The one line under the Usage tab's plan limits.
pub const USAGE_NOTE: &str = "Read with the sign-ins your agent CLIs already have: Arakun has no account of its own and never asks you to approve access. It only reads them, sends each one only to its own provider, and never changes them. A Factory API key you connect instead (kept in the system keyring), or FACTORY_API_KEY, goes only to api.factory.ai.";

/// The hosts a CLI's sign-in may be sent to.
pub fn hosts(agent: AgentKind) -> &'static [&'static str] {
    agent.spec().sign_in_hosts
}

/// Fails unless `url` is https to one of `agent`'s [`hosts`]. Call it before
/// a CLI's sign-in goes into a request.
pub fn check(agent: AgentKind, url: &str) -> Result<()> {
    let host = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or_default();
    // A port, a user name or another host fails the exact match.
    if hosts(agent).iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return Ok(());
    }
    bail!(
        "Arakun sends {}'s sign-in only to its provider, not to {}",
        agent.label(),
        crate::redact::value(if host.is_empty() { url } else { host })
    )
}

/// Why there is no Arakun account or consent screen, for the agent's section
/// in Settings. It sits under the section's Sign in or Connect button, so it
/// says what that button runs.
pub fn notice(agent: AgentKind) -> &'static str {
    agent.spec().sign_in_notice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_only_to_the_providers_own_host() {
        assert!(
            check(
                AgentKind::Claude,
                "https://api.anthropic.com/api/oauth/usage"
            )
            .is_ok()
        );
        assert!(
            check(
                AgentKind::Droid,
                "https://api.factory.ai/api/billing/limits"
            )
            .is_ok()
        );
        assert!(check(AgentKind::OpenCode, "https://opencode.ai/zen/go/v1/usage").is_ok());
        assert!(
            check(
                AgentKind::WorkersAi,
                "https://api.cloudflare.com/client/v4/x"
            )
            .is_ok()
        );
        for url in [
            "http://api.anthropic.com/api/oauth/usage",
            "https://riks-gateway.example.ts.net:8443/v1/messages",
            "https://api.anthropic.com.evil.test/",
            "https://api.anthropic.com@evil.test/",
            "https://api.anthropic.com:8443/",
            "https://evil.test/?https://api.anthropic.com",
            "api.anthropic.com/v1/messages",
        ] {
            assert!(check(AgentKind::Claude, url).is_err(), "{url}");
        }
        assert!(check(AgentKind::Claude, "https://api.factory.ai/").is_err());
        assert!(check(AgentKind::Codex, "https://api.openai.com/").is_err());
        assert!(check(AgentKind::Devin, "https://server.codeium.com/exa.x/Y").is_ok());
        assert!(check(AgentKind::Devin, "https://api.devin.ai/").is_err());
    }

    #[test]
    fn every_notice_names_where_the_sign_in_goes() {
        for agent in [
            AgentKind::Claude,
            AgentKind::Droid,
            AgentKind::OpenCode,
            AgentKind::WorkersAi,
            AgentKind::Devin,
        ] {
            for host in hosts(agent) {
                assert!(notice(agent).contains(host), "{agent:?} {host}");
            }
        }
        assert!(notice(AgentKind::Codex).contains("never reads the tokens"));
        assert!(notice(AgentKind::Cursor).contains("never reads the tokens"));
        // Droid can run on a key Arakun keeps, not only on droid's login.
        assert!(notice(AgentKind::Droid).contains("keyring"));
        for word in ["keyring", "FACTORY_API_KEY", "api.factory.ai"] {
            assert!(USAGE_NOTE.contains(word), "{word}");
        }
    }
}
