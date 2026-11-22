//! What lives on this machine: the agents' MCP config, the wrangler configs of
//! the projects, and the `cloudflared` command that shares a local server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

use super::MCP_URL;
use crate::agent::AgentKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mcp {
    /// The agent has no Cloudflare MCP server configured.
    Missing,
    /// Configured; the agent hasn't said yet whether it connects.
    Added,
    NeedsAuth,
    Connected,
    /// Configured but failing, with what the agent said.
    Failing(String),
    /// The agent's CLI isn't installed, so it can't add the server or sign in.
    /// Only Codex says so: the other agents need their CLI to run at all.
    NoCli,
}

/// What the agent's config alone says, without asking the agent: quick
/// enough to show before `mcp_status` answers.
pub fn mcp_configured(agent: AgentKind) -> Mcp {
    if agent == AgentKind::Codex && crate::agent::codex_cli().is_none() {
        Mcp::NoCli
    } else if mcp_server(agent).is_some() {
        Mcp::Added
    } else {
        Mcp::Missing
    }
}

/// Whether `agent`'s config has Cloudflare's MCP server: Droid's
/// `~/.factory/mcp.json`, Claude Code's `~/.claude.json` (user servers),
/// OpenCode's global `opencode.json`/`opencode.jsonc` (its `mcp` key), Codex's
/// `~/.codex/config.toml`.
pub fn mcp_server(agent: AgentKind) -> Option<String> {
    // The Cloudflare view asks every few seconds whether this changed. Parse
    // the config only when one of its files did: `~/.claude.json` alone is
    // often over 100 KB.
    type Stamps = Vec<Option<(std::time::SystemTime, u64)>>;
    type Seen = HashMap<AgentKind, (Stamps, Option<String>)>;
    static SEEN: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    let stamps: Stamps = mcp_files(agent)
        .iter()
        .map(|f| {
            let m = std::fs::metadata(f).ok()?;
            Some((m.modified().ok()?, m.len()))
        })
        .collect();
    if let Some((seen, found)) = SEEN
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&agent)).cloned())
        && seen == stamps
    {
        return found;
    }
    let found = read_mcp_server(agent);
    if let Ok(mut g) = SEEN.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(agent, (stamps, found.clone()));
    }
    found
}

/// The files `agent`'s MCP servers are configured in.
fn mcp_files(agent: AgentKind) -> Vec<PathBuf> {
    let home = crate::platform::home();
    match agent {
        AgentKind::Droid => vec![home.join(".factory").join("mcp.json")],
        AgentKind::Claude => vec![home.join(".claude.json")],
        AgentKind::OpenCode => {
            let config = opencode_config_dir();
            vec![config.join("opencode.json"), config.join("opencode.jsonc")]
        }
        AgentKind::Codex => vec![codex_home().join("config.toml")],
        AgentKind::WorkersAi | AgentKind::Devin | AgentKind::Cursor => Vec::new(),
    }
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::platform::home().join(".codex"))
}

fn opencode_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::platform::home().join(".config"))
        .join("opencode")
}

fn read_mcp_server(agent: AgentKind) -> Option<String> {
    let home = crate::platform::home();
    let file = match agent {
        AgentKind::Droid => home.join(".factory").join("mcp.json"),
        AgentKind::Claude => home.join(".claude.json"),
        AgentKind::OpenCode => return opencode_mcp_server(),
        AgentKind::Codex => {
            let toml = std::fs::read_to_string(codex_home().join("config.toml")).ok()?;
            return codex_mcp_server(&toml);
        }
        // arakun's own agent has no MCP client. Devin and Cursor have one,
        // but Arakun does not read or change their MCP config yet.
        AgentKind::WorkersAi | AgentKind::Devin | AgentKind::Cursor => return None,
    };
    let v: Value = crate::durable::load_json(file)?;
    v["mcpServers"].as_object()?.iter().find_map(|(name, s)| {
        (s["url"]
            .as_str()
            .is_some_and(|u| u.contains("mcp.cloudflare.com"))
            && s["disabled"].as_bool() != Some(true))
        .then(|| name.clone())
    })
}

/// OpenCode keeps remote servers as `mcp.<name> = {type: "remote", url, enabled}`.
/// A `.jsonc` with comments in it does not parse here and counts as missing.
fn opencode_mcp_server() -> Option<String> {
    let config = opencode_config_dir();
    ["opencode.json", "opencode.jsonc"].iter().find_map(|f| {
        let v: Value = crate::durable::load_json(config.join(f))?;
        v["mcp"].as_object()?.iter().find_map(|(name, s)| {
            (s["url"]
                .as_str()
                .is_some_and(|u| u.contains("mcp.cloudflare.com"))
                && s["enabled"].as_bool() != Some(false))
            .then(|| name.clone())
        })
    })
}

/// Codex keeps servers as `[mcp_servers.<name>]` tables with `url` and `enabled`.
/// A line scan covers what `codex mcp add` writes, without a TOML parser: the
/// first enabled server with Cloudflare's URL.
fn codex_mcp_server(toml: &str) -> Option<String> {
    // (name, has Cloudflare's URL, enabled) of the table being read.
    let mut current: Option<(String, bool, bool)> = None;
    let done = |c: Option<(String, bool, bool)>| {
        c.filter(|(_, cloudflare, enabled)| *cloudflare && *enabled)
            .map(|(name, ..)| name)
    };
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            if let Some(name) = done(current.take()) {
                return Some(name);
            }
            current = codex_server_header(line).map(|n| (n, false, true));
            continue;
        }
        let Some((_, cloudflare, enabled)) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Neither a URL nor a boolean holds a `#`, so it starts a comment.
        let value = value.split('#').next().unwrap_or_default().trim();
        match key.trim() {
            "url" => *cloudflare = value.contains("mcp.cloudflare.com"),
            "enabled" => *enabled = value != "false",
            _ => {}
        }
    }
    done(current)
}

/// The server a `[mcp_servers.<name>]` header opens, bare or quoted, with an
/// optional `# comment` after it. `None` for any other table, such as a
/// server's `[mcp_servers.<name>.env]`.
fn codex_server_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("[mcp_servers.")?;
    let (name, after) = match rest.chars().next()? {
        q @ ('"' | '\'') => {
            let body = &rest[1..];
            let end = body.find(q)?;
            (&body[..end], &body[end + 1..])
        }
        _ => {
            let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))?;
            (&rest[..end], &rest[end..])
        }
    };
    let after = after.strip_prefix(']')?.trim_start();
    (!name.is_empty() && (after.is_empty() || after.starts_with('#'))).then(|| name.to_string())
}

/// The `codex` to run: the user's, else the one installed with the adapter.
/// Quoted for the terminal's shell.
fn codex_program() -> String {
    match crate::agent::codex_cli() {
        Some(bin) => {
            let bin = shell_word(&bin.display().to_string());
            // PowerShell runs a quoted path only after `&`.
            if cfg!(windows) && bin.starts_with('\'') {
                format!("& {bin}")
            } else {
                bin
            }
        }
        None => "codex".into(),
    }
}

/// `s` as one word for the terminal's shell: sh, or PowerShell on Windows.
fn shell_word(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/@".contains(c) || (cfg!(windows) && c == '\\'))
    {
        s.to_string()
    } else if cfg!(windows) {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `codex mcp list --json` says whether each server is signed in; it does not
/// try to connect.
fn codex_list_status(json: &str, name: &str) -> Mcp {
    let Ok(list) = serde_json::from_str::<Value>(json) else {
        return Mcp::Added;
    };
    let Some(server) = list
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["name"] == name)
    else {
        return Mcp::Added;
    };
    match server["auth_status"].as_str() {
        Some("not_logged_in") => Mcp::NeedsAuth,
        Some("o_auth" | "bearer_token") => Mcp::Connected,
        _ => Mcp::Added,
    }
}

/// Asks the agent whether the Cloudflare server connects (it tries every server: 1-10 s).
pub fn mcp_status(agent: AgentKind) -> Mcp {
    if agent == AgentKind::Codex && crate::agent::codex_cli().is_none() {
        return Mcp::NoCli;
    }
    let Some(name) = mcp_server(agent) else {
        return Mcp::Missing;
    };
    let bin = match agent {
        AgentKind::Droid => "droid",
        AgentKind::Claude => "claude",
        AgentKind::OpenCode => "opencode",
        AgentKind::Codex => "codex",
        AgentKind::WorkersAi | AgentKind::Devin | AgentKind::Cursor => return Mcp::Missing,
    };
    let bin = match agent {
        AgentKind::Codex => crate::agent::codex_cli(),
        _ => crate::agent::which(bin),
    }
    .unwrap_or_else(|| bin.into());
    let json = (agent == AgentKind::Codex).then_some("--json");
    let out = crate::platform::command(bin)
        .args(["mcp", "list"])
        .args(json)
        .stdin(Stdio::null())
        .output();
    match (out, agent) {
        (Ok(o), AgentKind::Droid) => mcp_line_status(&String::from_utf8_lossy(&o.stdout), &name),
        (Ok(o), AgentKind::Claude) => {
            claude_line_status(&String::from_utf8_lossy(&o.stdout), &name)
        }
        (Ok(o), AgentKind::OpenCode) => {
            opencode_line_status(&String::from_utf8_lossy(&o.stdout), &name)
        }
        (Ok(o), AgentKind::Codex) => codex_list_status(&String::from_utf8_lossy(&o.stdout), &name),
        (Ok(_), AgentKind::WorkersAi | AgentKind::Devin | AgentKind::Cursor) => Mcp::Missing,
        (Err(_), _) => Mcp::Added,
    }
}

/// The state on `name`'s line of `droid mcp list` (`  cloudflare  http  needs authentication  [user]`).
fn mcp_line_status(list: &str, name: &str) -> Mcp {
    let Some(line) = list
        .lines()
        .map(str::trim)
        .find(|l| l.split_whitespace().next() == Some(name))
    else {
        return Mcp::Added;
    };
    if line.contains("needs auth") {
        Mcp::NeedsAuth
    } else if line.split_whitespace().any(|w| w == "connected") {
        Mcp::Connected
    } else {
        let state = line
            .split_whitespace()
            .skip(2)
            .take_while(|w| !w.starts_with('['))
            .collect::<Vec<_>>()
            .join(" ");
        Mcp::Failing(state)
    }
}

/// The state on `name`'s line of `claude mcp list` (`cloudflare: https://… (HTTP) - ✔ Connected`).
fn claude_line_status(list: &str, name: &str) -> Mcp {
    let Some(line) = list
        .lines()
        .map(str::trim)
        .find(|l| l.split(':').next() == Some(name))
    else {
        return Mcp::Added;
    };
    let state = line
        .rsplit(" - ")
        .next()
        .unwrap_or_default()
        .trim_start_matches(|c: char| !c.is_alphabetic())
        .to_string();
    if state.starts_with("Connected") {
        Mcp::Connected
    } else if state.starts_with("Needs auth") {
        Mcp::NeedsAuth
    } else {
        Mcp::Failing(state.to_lowercase())
    }
}

/// The state on `name`'s line of `opencode mcp list` (`●  ✓ cloudflare connected`,
/// colored), its error on the next line when it failed.
fn opencode_line_status(list: &str, name: &str) -> Mcp {
    let plain: String = {
        let mut out = String::new();
        let mut chars = list.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|c| c.is_ascii_alphabetic());
            } else {
                out.push(c);
            }
        }
        out
    };
    let Some(line) = plain
        .lines()
        .find(|l| l.split_whitespace().any(|w| w == name))
    else {
        return Mcp::Added;
    };
    let state = line
        .split_whitespace()
        .skip_while(|w| *w != name)
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    if state.starts_with("connected") {
        Mcp::Connected
    } else if state.contains("auth") {
        Mcp::NeedsAuth
    } else {
        Mcp::Failing(state)
    }
}

/// What the Connect button runs in a terminal: add the server if needed, then sign in.
pub fn mcp_connect_command(agent: AgentKind, state: &Mcp) -> String {
    match agent {
        AgentKind::Droid => {
            let auth = "droid mcp auth";
            match state {
                Mcp::Missing => format!("droid mcp add cloudflare {MCP_URL} --type http; {auth}"),
                _ => auth.to_string(),
            }
        }
        // Claude Code signs in to HTTP servers from its /mcp menu.
        AgentKind::Claude => match state {
            Mcp::Missing => format!(
                "claude mcp add --scope user --transport http cloudflare {MCP_URL} && claude /mcp"
            ),
            _ => "claude /mcp".into(),
        },
        AgentKind::OpenCode => match state {
            Mcp::Missing => format!(
                "opencode mcp add cloudflare --url {MCP_URL} && opencode mcp auth cloudflare"
            ),
            _ => "opencode mcp auth cloudflare".into(),
        },
        // `codex mcp add` signs in to an OAuth server right away. The server
        // can have another name than `cloudflare`; sign in to the one found.
        AgentKind::Codex => {
            let name = mcp_server(AgentKind::Codex);
            codex_connect_command(&codex_program(), state, name.as_deref())
        }
        // Not supported: the shell tells the user instead of running this.
        AgentKind::WorkersAi | AgentKind::Devin | AgentKind::Cursor => String::new(),
    }
}

fn codex_connect_command(codex: &str, state: &Mcp, name: Option<&str>) -> String {
    match (state, name) {
        (Mcp::Missing, _) | (_, None) => format!("{codex} mcp add cloudflare --url {MCP_URL}"),
        (_, Some(name)) => format!("{codex} mcp login {}", shell_word(name)),
    }
}

/// The wrangler configs in `dir` or one folder below it (monorepos keep one per app).
fn wrangler_files(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut dirs = vec![dir.to_path_buf()];
    for parent in [
        dir.to_path_buf(),
        dir.join("apps"),
        dir.join("workers"),
        dir.join("packages"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&parent) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if e.path().is_dir()
                    && !name.starts_with('.')
                    && name != "node_modules"
                    && name != "target"
                {
                    dirs.push(e.path());
                }
            }
        }
    }
    let mut files = Vec::new();
    for d in dirs {
        for file in ["wrangler.toml", "wrangler.jsonc", "wrangler.json"] {
            let path = d.join(file);
            if path.is_file() {
                files.push((path, file.ends_with(".toml")));
            }
        }
    }
    files
}

/// The names of the Workers whose wrangler config sits in `dir` or one folder below it.
pub fn workers_in(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for (file, toml) in wrangler_files(dir) {
        if let Ok(text) = std::fs::read_to_string(&file)
            && let Some(n) = wrangler_name(&text, toml)
            && !names.contains(&n)
        {
            names.push(n);
        }
    }
    names
}

/// D1 database id → name, from the wrangler configs of `roots`.
pub fn d1_names(roots: &[PathBuf]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for root in roots {
        for (file, _) in wrangler_files(root) {
            if let Ok(text) = std::fs::read_to_string(&file) {
                names.extend(d1_pairs(&text));
            }
        }
    }
    names
}

/// `(database_id, database_name)` pairs of a wrangler config, TOML or JSON: each
/// id goes with the closest `database_name` around it (they share a block).
fn d1_pairs(text: &str) -> Vec<(String, String)> {
    let values = |key: &str| -> Vec<(usize, String)> {
        text.match_indices(key)
            .filter_map(|(at, _)| {
                let rest = text[at + key.len()..].trim_start_matches('"').trim_start();
                let rest = rest
                    .strip_prefix('=')
                    .or_else(|| rest.strip_prefix(':'))?
                    .trim_start();
                let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
                let rest = &rest[1..];
                let end = rest.find(quote)?;
                Some((at, rest[..end].to_string())).filter(|(_, v)| !v.is_empty())
            })
            .collect()
    };
    let names = values("database_name");
    values("database_id")
        .into_iter()
        .filter_map(|(at, id)| {
            let (_, name) = names
                .iter()
                .filter(|(n, _)| n.abs_diff(at) < 400)
                .min_by_key(|(n, _)| n.abs_diff(at))?;
            Some((id, name.clone()))
        })
        .collect()
}

/// The top-level `name` of a wrangler config.
fn wrangler_name(text: &str, toml: bool) -> Option<String> {
    if toml {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                return None;
            }
            if let Some(rest) = line.strip_prefix("name")
                && let Some(value) = rest.trim_start().strip_prefix('=')
            {
                let v = value.trim().trim_matches(|c| c == '"' || c == '\'');
                return (!v.is_empty()).then(|| v.to_string());
            }
        }
        return None;
    }
    // The first "name" key: wrangler writes it near the top, before any nested block.
    let at = text.find("\"name\"")?;
    let rest = text[at + 6..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string()).filter(|n| !n.is_empty())
}

/// Splits what was typed into the Share box into addresses (or `*@domain` wildcards).
pub fn share_emails(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|e| e.contains('@') && !e.ends_with('@'))
        // They go on a shell command line.
        .filter(|e| {
            e.chars()
                .all(|c| c.is_ascii_alphanumeric() || "@.*_+-".contains(c))
        })
        .map(str::to_string)
        .collect()
}

/// The command that shares `port` through a Quick Tunnel only `emails` can open.
pub fn share_command(port: u16, emails: &[String]) -> String {
    // Quoted so a `*@domain` wildcard isn't expanded by the shell.
    format!(
        "cloudflared tunnel --url http://localhost:{port} --allowed-mail '{}'",
        emails.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_config_names_the_cloudflare_server() {
        let toml = "model = \"gpt\"\n\n[mcp_servers.docs]\nurl = \"https://example.com/mcp\"\n\n\
                    [mcp_servers.cloudflare]\nurl = \"https://mcp.cloudflare.com/mcp\"\n\n\
                    [mcp_servers.cloudflare.env]\nA = \"b\"\n";
        assert_eq!(codex_mcp_server(toml).as_deref(), Some("cloudflare"));
        let off =
            "[mcp_servers.\"cf\"]\nurl = \"https://mcp.cloudflare.com/mcp\"\nenabled = false\n";
        assert_eq!(codex_mcp_server(off), None);
        assert_eq!(codex_mcp_server("[mcp_servers.docs]\nurl = \"x\"\n"), None);
    }

    #[test]
    fn codex_config_skips_a_disabled_server() {
        let toml = "[mcp_servers.old]\nurl = \"https://mcp.cloudflare.com/mcp\"\nenabled = false\n\n\
                    [mcp_servers.cf]\nurl = \"https://mcp.cloudflare.com/mcp\"\n";
        assert_eq!(codex_mcp_server(toml).as_deref(), Some("cf"));
    }

    #[test]
    fn codex_config_allows_trailing_comments() {
        let toml = "[mcp_servers.cloudflare] # work account\nurl = \"https://mcp.cloudflare.com/mcp\" # oauth\n";
        assert_eq!(codex_mcp_server(toml).as_deref(), Some("cloudflare"));
        let quoted = "[mcp_servers.'cf'] # quoted\nurl = \"https://mcp.cloudflare.com/mcp\"\n";
        assert_eq!(codex_mcp_server(quoted).as_deref(), Some("cf"));
        let paused = "[mcp_servers.cloudflare]\nurl = \"https://mcp.cloudflare.com/mcp\"\nenabled = false # paused\n";
        assert_eq!(codex_mcp_server(paused), None);
        assert_eq!(
            codex_server_header("[mcp_servers.cloudflare.env] # x"),
            None
        );
        assert_eq!(codex_server_header("[mcp_servers.cloudflare] x"), None);
    }

    #[test]
    fn codex_connect_signs_in_to_the_found_server() {
        assert_eq!(
            codex_connect_command("codex", &Mcp::NeedsAuth, Some("cf")),
            "codex mcp login cf"
        );
        assert_eq!(
            codex_connect_command("codex", &Mcp::Missing, None),
            format!("codex mcp add cloudflare --url {MCP_URL}")
        );
        assert_eq!(
            codex_connect_command("codex", &Mcp::NeedsAuth, Some("my cf")),
            "codex mcp login 'my cf'"
        );
    }

    #[test]
    fn codex_list_reads_the_sign_in() {
        let list = |auth: &str| {
            format!(r#"[{{"name": "cloudflare", "enabled": true, "auth_status": "{auth}"}}]"#)
        };
        assert_eq!(
            codex_list_status(&list("not_logged_in"), "cloudflare"),
            Mcp::NeedsAuth
        );
        assert_eq!(
            codex_list_status(&list("o_auth"), "cloudflare"),
            Mcp::Connected
        );
        assert_eq!(codex_list_status(&list("o_auth"), "other"), Mcp::Added);
        assert_eq!(codex_list_status("not json", "cloudflare"), Mcp::Added);
    }

    #[test]
    fn mcp_list_lines() {
        let list = "Configured MCP servers:\n  acme  http  connected  [user]\n  cloudflare  http  needs authentication  [user]\n  cf2  http  failed  [user]\n";
        assert_eq!(mcp_line_status(list, "cloudflare"), Mcp::NeedsAuth);
        assert_eq!(mcp_line_status(list, "acme"), Mcp::Connected);
        assert_eq!(mcp_line_status(list, "cf2"), Mcp::Failing("failed".into()));
        assert_eq!(mcp_line_status(list, "other"), Mcp::Added);
        assert_eq!(
            mcp_connect_command(AgentKind::Droid, &Mcp::Missing),
            "droid mcp add cloudflare https://mcp.cloudflare.com/mcp --type http; droid mcp auth"
        );
        let claude = "Checking MCP server health…\n\ncloudflare: https://mcp.cloudflare.com/mcp (HTTP) - ! Needs authentication\nposthog: https://x (HTTP) - ✔ Connected\nbad: https://y (HTTP) - ✗ Failed to connect\n";
        assert_eq!(claude_line_status(claude, "cloudflare"), Mcp::NeedsAuth);
        assert_eq!(claude_line_status(claude, "posthog"), Mcp::Connected);
        assert_eq!(
            claude_line_status(claude, "bad"),
            Mcp::Failing("failed to connect".into())
        );
        assert_eq!(claude_line_status(claude, "other"), Mcp::Added);
        let opencode = "\x1b[0m\n┌  MCP Servers\n│\n●  ⚠ cloudflare \x1b[90mneeds authentication\n│      \x1b[90mhttps://mcp.cloudflare.com/mcp\n●  ✓ posthog \x1b[90mconnected\n●  ✗ pencil \x1b[90mfailed\n";
        assert_eq!(opencode_line_status(opencode, "cloudflare"), Mcp::NeedsAuth);
        assert_eq!(opencode_line_status(opencode, "posthog"), Mcp::Connected);
        assert_eq!(
            opencode_line_status(opencode, "pencil"),
            Mcp::Failing("failed".into())
        );
        assert_eq!(opencode_line_status(opencode, "other"), Mcp::Added);
    }

    #[test]
    fn wrangler_names() {
        assert_eq!(
            wrangler_name(
                "# x\nname = \"blog\"\nmain = \"a\"\n[vars]\nname = \"no\"",
                true
            )
            .as_deref(),
            Some("blog")
        );
        assert_eq!(wrangler_name("[vars]\nname = \"no\"", true), None);
        assert_eq!(
            wrangler_name(
                "{\n  // c\n  \"name\": \"example-viewers\",\n  \"main\": \"x\"\n}",
                false
            )
            .as_deref(),
            Some("example-viewers")
        );
    }

    #[test]
    fn d1_ids_get_their_names() {
        let toml = "[[d1_databases]]\nbinding = \"DB\"\ndatabase_name = \"auth\"\ndatabase_id = \"5ef5\"\n\n[[d1_databases]]\nbinding = \"LOGS\"\ndatabase_id = \"aa11\"\ndatabase_name = \"logs\"\n";
        assert_eq!(
            d1_pairs(toml),
            vec![
                ("5ef5".into(), "auth".into()),
                ("aa11".into(), "logs".into())
            ]
        );
        let json = "{ \"d1_databases\": [ { \"binding\": \"DB\", \"database_name\": \"prod\", \"database_id\": \"2b01\" } ] }";
        assert_eq!(d1_pairs(json), vec![("2b01".into(), "prod".into())]);
    }

    #[test]
    fn share() {
        assert_eq!(
            share_emails("a@x.dev, *@example.com b nope@ x'@y.dev"),
            vec!["a@x.dev".to_string(), "*@example.com".to_string()]
        );
        assert_eq!(
            share_command(3000, &["a@x.dev".into(), "*@c.dev".into()]),
            "cloudflared tunnel --url http://localhost:3000 --allowed-mail 'a@x.dev,*@c.dev'"
        );
    }
}
