//! Droid's skills and MCP servers, read and toggled through a short-lived
//! `droid exec` in the environment's folder (project-level ones depend on it).
//! None of these calls need a session, so nothing lands in Droid's session store;
//! toggles do write Droid's user settings, as its own CLI does.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use super::launch;
use super::rpc::{FACTORY_API_VERSION, FACTORY_PROTOCOL_VERSION};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct McpServer {
    pub name: String,
    /// user, project, plugin, ...
    pub source: String,
    /// stdio, http or sse.
    pub server_type: String,
    pub status: String,
    pub managed: bool,
    pub tools: usize,
}

impl McpServer {
    pub fn enabled(&self) -> bool {
        self.status != "disabled"
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// personal, project, ...
    pub location: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Extensions {
    pub servers: Vec<McpServer>,
    pub skills: Vec<Skill>,
}

const LISTS: [&str; 3] = [
    "droid.list_mcp_tools",
    "droid.list_skills",
    "droid.list_mcp_servers",
];

pub fn load(cwd: &Path) -> Result<Extensions> {
    run(cwd, None)
}

pub fn set_server(cwd: &Path, name: &str, enabled: bool) -> Result<Extensions> {
    let params = json!({"serverName": name, "enabled": enabled, "settingsLevel": "user"});
    run(cwd, Some(("droid.toggle_mcp_server", params)))
}

pub fn set_skill(cwd: &Path, name: &str, enabled: bool) -> Result<Extensions> {
    let params = json!({"skillName": name, "disabled": !enabled, "settingsLevel": "user"});
    run(cwd, Some(("droid.set_skill_disabled", params)))
}

fn cache_file(cwd: &Path) -> std::path::PathBuf {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    crate::platform::app_dir()
        .join("cache")
        .join("extensions")
        .join(format!("droid-{:016x}.json", h.finish()))
}

/// What the last `load` found in `cwd`, to show while a new one runs.
pub fn cached(cwd: &Path) -> Option<Extensions> {
    serde_json::from_slice(&std::fs::read(cache_file(cwd)).ok()?).ok()
}

fn save(cwd: &Path, x: &Extensions) {
    let file = cache_file(cwd);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(x) {
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, file);
        }
    }
}

/// Runs `first` (if any) and then the three lists, in order, on one process.
fn run(cwd: &Path, first: Option<(&str, Value)>) -> Result<Extensions> {
    let x = run_lists(cwd, first)?;
    save(cwd, &x);
    Ok(x)
}

fn run_lists(cwd: &Path, first: Option<(&str, Value)>) -> Result<Extensions> {
    let mut calls: Vec<(&str, Value)> = first.into_iter().collect();
    calls.extend(LISTS.iter().map(|m| (*m, json!({}))));
    let mut replies = batch(cwd, &calls)?;
    if calls.len() > LISTS.len() {
        let reply = replies.remove(0);
        if let Err(e) = reply {
            return Err(anyhow!("{} failed: {e}", calls[0].0));
        }
    }
    let [tools, skills, servers]: [Result<Value, String>; 3] =
        replies.try_into().map_err(|_| anyhow!("missing replies"))?;
    let servers = servers.map_err(|e| anyhow!("listing MCP servers failed: {e}"))?;
    let skills = skills.map_err(|e| anyhow!("listing skills failed: {e}"))?;
    Ok(parse(&tools.unwrap_or(Value::Null), &skills, &servers))
}

fn parse(tools: &Value, skills: &Value, servers: &Value) -> Extensions {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for t in tools["tools"].as_array().into_iter().flatten() {
        *counts.entry(s(t, "serverName")).or_default() += 1;
    }
    let mut servers: Vec<McpServer> = servers["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| McpServer {
            name: s(v, "name"),
            source: s(v, "source"),
            server_type: s(v, "serverType"),
            status: s(v, "status"),
            managed: v["isManaged"].as_bool().unwrap_or(false),
            tools: counts
                .get(v["name"].as_str().unwrap_or_default())
                .copied()
                .unwrap_or(0),
        })
        .collect();
    servers.sort_by_key(|a| a.name.to_lowercase());
    let mut skills: Vec<Skill> = skills["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| Skill {
            name: s(v, "name"),
            description: s(v, "description"),
            location: s(v, "location"),
            enabled: v["enabled"].as_bool().unwrap_or(true),
        })
        .collect();
    skills.sort_by_key(|a| a.name.to_lowercase());
    Extensions { servers, skills }
}

/// Sends every call at once and waits for all the answers (or a timeout).
fn batch(cwd: &Path, calls: &[(&str, Value)]) -> Result<Vec<Result<Value, String>>> {
    let spec = launch::resolve_droid_native(None)?;
    let mut child = crate::platform::command(&spec.program)
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", spec.display()))?;
    let mut stdin = child.stdin.take().context("droid stdin")?;
    let stdout = child.stdout.take().context("droid stdout")?;
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v["type"].as_str() == Some("response")
                && tx.send(v).is_err()
            {
                break;
            }
        }
    });
    let mut wrote = Ok(());
    for (i, (method, params)) in calls.iter().enumerate() {
        let request = json!({
            "jsonrpc": "2.0",
            "type": "request",
            "factoryApiVersion": FACTORY_API_VERSION,
            "factoryProtocolVersion": FACTORY_PROTOCOL_VERSION,
            "id": format!("arakun-ext-{i}"),
            "method": method,
            "params": params,
        });
        wrote = wrote.and_then(|_| writeln!(stdin, "{request}"));
    }
    let wrote = wrote.and_then(|_| stdin.flush());
    let mut replies: Vec<Option<Result<Value, String>>> = vec![None; calls.len()];
    let deadline = Instant::now() + Duration::from_secs(30);
    if wrote.is_ok() {
        while replies.iter().any(Option::is_none) {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(v) = rx.recv_timeout(left) else { break };
            let Some(i) = v["id"]
                .as_str()
                .and_then(|id| id.strip_prefix("arakun-ext-"))
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|&i| i < calls.len())
            else {
                continue;
            };
            replies[i] = Some(match v.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                None => Ok(v["result"].clone()),
            });
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    wrote.context("writing to droid")?;
    Ok(replies
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("no answer".into())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the `droid` CLI: `cargo test live_load -- --ignored`.
    #[test]
    #[ignore]
    fn live_load() {
        let e = load(&std::env::temp_dir()).unwrap();
        eprintln!("{} servers, {} skills", e.servers.len(), e.skills.len());
        for s in &e.servers {
            eprintln!("  {s:?}");
        }
    }

    #[test]
    fn parses_lists() {
        let tools =
            json!({"tools": [{"serverName": "gh"}, {"serverName": "gh"}, {"serverName": "x"}]});
        let skills = json!({"skills": [
            {"name": "b", "description": "B", "location": "personal", "enabled": false},
            {"name": "a", "description": "A", "location": "project", "enabled": true}
        ]});
        let servers = json!({"servers": [
            {"name": "gh", "source": "user", "serverType": "http", "status": "connected", "isManaged": false},
            {"name": "Docs", "source": "project", "serverType": "stdio", "status": "disabled"}
        ]});
        let e = parse(&tools, &skills, &servers);
        assert_eq!(e.servers[0].name, "Docs");
        assert!(!e.servers[0].enabled());
        assert_eq!(e.servers[1].tools, 2);
        assert_eq!(e.skills[0].name, "a");
        assert!(!e.skills[1].enabled);
    }
}
