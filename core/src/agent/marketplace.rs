//! Where to find more: the Claude Code plugins its marketplaces offer (from
//! `claude plugin list --available` and the marketplaces' own manifests) and
//! the official MCP Registry (registry.modelcontextprotocol.io), with what it
//! takes to install an entry of each in Claude Code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use super::launch;

const REGISTRY: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
/// Installing clones the plugin's repository.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------
// Plugins.

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PluginEntry {
    /// `name@marketplace`.
    pub id: String,
    pub name: String,
    pub description: String,
    pub marketplace: String,
    pub category: Option<String>,
    pub installs: u64,
    pub homepage: Option<String>,
    pub installed: bool,
    /// Who makes it: Anthropic, the company it's for, or its author.
    #[serde(default)]
    pub publisher: Option<String>,
    /// Made by Anthropic or by a known company itself (`publishers`).
    #[serde(default)]
    pub official: bool,
}

fn cache_file() -> PathBuf {
    crate::paths::shared("cache")
        .join("extensions")
        .join("plugin-catalog.json")
}

/// The catalog as last read, to show at once.
pub fn cached_plugins() -> Option<Vec<PluginEntry>> {
    crate::durable::load_json(cache_file())
}

/// Blocking: every plugin the marketplaces offer, most installed first.
pub fn plugins(cwd: &Path) -> Result<Vec<PluginEntry>> {
    let bin = launch::which("claude").ok_or_else(|| anyhow!("claude is not on PATH"))?;
    let out = run(
        &bin,
        cwd,
        &["plugin", "list", "--available", "--json"],
        CLI_TIMEOUT,
    )?;
    let v: Value = serde_json::from_str(&out).context("reading claude plugin list")?;
    let list = parse_plugins(&v, &manifests());
    crate::durable::cache_json(&cache_file(), &list);
    Ok(list)
}

#[derive(Clone, Default)]
struct Manifest {
    category: Option<String>,
    homepage: Option<String>,
    publisher: Option<String>,
    official: bool,
}

/// Who publishes a plugin, from its manifest entry: Anthropic's own live in the
/// marketplace's `./plugins`; a company's own come from its GitHub organization
/// or name it as author.
fn publisher(p: &Value, name: &str) -> (Option<String>, bool) {
    use super::publishers::{by_name, by_org, github_owner};
    let source = match &p["source"] {
        Value::String(s) => s.clone(),
        v => v["url"].as_str().unwrap_or_default().to_string(),
    };
    if source.starts_with("./plugins/") {
        return (Some("Anthropic".into()), true);
    }
    let author = p["author"]["name"]
        .as_str()
        .or(p["author"].as_str())
        .map(str::to_string);
    let owner = github_owner(&source).map(str::to_string);
    // Partner plugins Anthropic keeps in its own repo, named after the partner.
    if source.starts_with("./external_plugins/")
        && let Some(company) = by_org(name).or_else(|| by_name(name))
    {
        return (Some(company.into()), true);
    }
    if let Some(company) = author
        .as_deref()
        .and_then(by_name)
        .or_else(|| owner.as_deref().and_then(by_org))
    {
        return (Some(company.into()), true);
    }
    // An author whose repository lives in its own organization (42Crunch, 42Crunch-AI).
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
    };
    // A plugin named after the organization it lives in (expo/expo, coderabbitai/coderabbit).
    let named = owner.as_deref().is_some_and(|o| {
        let (o, n) = (norm(o), norm(name));
        o.len() >= 3 && !o.ends_with("community") && (n.starts_with(&o) || o.starts_with(&n))
    });
    let own = named
        || match (&author, &owner) {
            (Some(a), Some(o)) => {
                let (a, o) = (norm(a), norm(o));
                a.len() >= 3
                    && !o.ends_with("community")
                    && (o.starts_with(&a[..a.len().min(5)]) || a.starts_with(&o[..o.len().min(5)]))
            }
            _ => false,
        };
    (author.or(owner), own)
}

/// Category, homepage and publisher by `name@marketplace`, from each marketplace's manifest.
fn manifests() -> HashMap<String, Manifest> {
    let mut out = HashMap::new();
    let dir = launch::home().join(".claude/plugins/marketplaces");
    for m in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let market = m.file_name().to_string_lossy().to_string();
        let file = m.path().join(".claude-plugin/marketplace.json");
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for p in v["plugins"].as_array().into_iter().flatten() {
            let Some(name) = p["name"].as_str() else {
                continue;
            };
            let s = |k: &str| p[k].as_str().map(str::to_string);
            let (publisher, official) = publisher(p, name);
            out.insert(
                format!("{name}@{market}"),
                Manifest {
                    category: s("category"),
                    homepage: s("homepage"),
                    publisher,
                    official,
                },
            );
        }
    }
    out
}

fn parse_plugins(v: &Value, manifests: &HashMap<String, Manifest>) -> Vec<PluginEntry> {
    let installed: Vec<&str> = v["installed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_str())
        .collect();
    let mut list: Vec<PluginEntry> = v["available"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = p["pluginId"].as_str()?.to_string();
            let Manifest {
                category,
                homepage,
                publisher,
                official,
            } = manifests.get(&id).cloned().unwrap_or_default();
            Some(PluginEntry {
                name: p["name"].as_str().unwrap_or(&id).to_string(),
                description: p["description"].as_str().unwrap_or_default().to_string(),
                marketplace: p["marketplaceName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                installs: p["installCount"].as_u64().unwrap_or(0),
                installed: installed.contains(&id.as_str()),
                category,
                homepage,
                publisher,
                official,
                id,
            })
        })
        .collect();
    list.sort_by(|a, b| {
        b.installs
            .cmp(&a.installs)
            .then_with(|| a.name.cmp(&b.name))
    });
    list
}

/// Installs a plugin for your user; it is on in every project.
pub fn install_plugin(cwd: &Path, id: &str) -> Result<()> {
    install_plugin_as(cwd, id, "user")
}

/// Installs a plugin in Claude Code's `scope` (`user`, `project` or `local`);
/// the last two are for the folder `cwd`.
pub fn install_plugin_as(cwd: &Path, id: &str, scope: &str) -> Result<()> {
    let bin = launch::which("claude").ok_or_else(|| anyhow!("claude is not on PATH"))?;
    super::claude_ext::run_cli(
        &bin,
        &scope_dir(cwd, scope),
        &["plugin", "install", id, "-s", scope],
        INSTALL_TIMEOUT,
    )
    .map(|_| ())
}

/// Where a `scope` install runs: a folder that is gone falls back to home only
/// for the user's, since the others write into the folder itself.
fn scope_dir(cwd: &Path, scope: &str) -> PathBuf {
    if scope == "user" {
        workdir(cwd)
    } else {
        cwd.to_path_buf()
    }
}

/// `1266133` → `1.3M`.
pub fn short_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

// ---------------------------------------------------------------------------
// The MCP Registry.

/// Something the user fills in to install: a header, an env var or a placeholder.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub secret: bool,
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Install {
    /// A server on the internet: Claude talks to its URL.
    Remote {
        transport: String,
        url: String,
        headers: Vec<Input>,
    },
    /// A program Claude starts: `command args…` with `env`.
    Local {
        kind: String,
        command: String,
        args: Vec<String>,
        env: Vec<Input>,
        placeholders: Vec<Input>,
    },
}

impl Install {
    pub fn inputs(&self) -> Vec<&Input> {
        match self {
            Install::Remote { headers, .. } => headers.iter().collect(),
            Install::Local {
                env, placeholders, ..
            } => placeholders.iter().chain(env.iter()).collect(),
        }
    }

    /// `remote`, `npm`, `pypi`, `docker`...
    pub fn kind(&self) -> &str {
        match self {
            Install::Remote { .. } => "remote",
            Install::Local { kind, .. } => kind,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegistryServer {
    /// `io.github.github/github-mcp-server`.
    pub name: String,
    pub title: String,
    pub description: String,
    pub version: String,
    pub website: Option<String>,
    pub repository: Option<String>,
    /// The ways to install it Arakun can do, best first.
    pub installs: Vec<Install>,
    /// The known company whose namespace it is published under (`publishers`).
    #[serde(default)]
    pub official: Option<String>,
}

impl RegistryServer {
    /// The name it gets in Claude: `github-mcp-server` → `github`.
    pub fn short_name(&self) -> String {
        let last = self
            .name
            .rsplit('/')
            .next()
            .unwrap_or(&self.name)
            .to_lowercase();
        let trimmed = last
            .trim_end_matches("-mcp-server")
            .trim_end_matches("-mcp")
            .trim_start_matches("mcp-")
            .trim_end_matches("-server");
        let base = if trimmed.is_empty() {
            last.as_str()
        } else {
            trimmed
        };
        base.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    }

    /// Who publishes it: `io.github.github` → `github`.
    pub fn publisher(&self) -> String {
        let ns = self.name.split('/').next().unwrap_or(&self.name);
        ns.strip_prefix("io.github.")
            .map(str::to_string)
            .unwrap_or_else(|| ns.split('.').rev().collect::<Vec<_>>().join("."))
    }
}

/// Blocking: one page of the registry, `query` matching names; the cursor of the next.
/// A one-word query also asks for its GitHub publisher's own servers, which sort
/// late by name (`io.github.github/…` after every `com.*` one).
pub fn search(query: &str, cursor: Option<&str>) -> Result<(Vec<RegistryServer>, Option<String>)> {
    let q = query.trim();
    let mut own = Vec::new();
    if cursor.is_none() && !q.is_empty() && q.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        own = fetch(&format!("io.github.{}/", q.to_lowercase()), None)
            .map(|(l, _)| l)
            .unwrap_or_default();
    }
    let (mut list, next) = fetch(q, cursor)?;
    list.retain(|s| !own.iter().any(|o| o.name == s.name));
    own.extend(list);
    rank(&mut own, q);
    Ok((own, next))
}

fn fetch(query: &str, cursor: Option<&str>) -> Result<(Vec<RegistryServer>, Option<String>)> {
    let mut url = format!("{REGISTRY}?limit=100&version=latest");
    if !query.trim().is_empty() {
        url.push_str(&format!("&search={}", encode(query.trim())));
    }
    if let Some(c) = cursor {
        url.push_str(&format!("&cursor={}", encode(c)));
    }
    let out = crate::platform::command("curl")
        .args(["-sS", "-m", "20", "-H", "Accept: application/json", &url])
        .stdin(Stdio::null())
        .output()
        .context("curl is needed to reach the MCP Registry")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .context("the MCP Registry answered something that is not JSON")?;
    Ok(parse_registry(&v))
}

/// The registry matches names by substring in name order; put the likely one first:
/// the publisher's own (`io.github.github/…` for "github"), an exact name, one with a repo.
fn rank(list: &mut [RegistryServer], query: &str) {
    let q = query.trim().to_lowercase();
    let score = |s: &RegistryServer| {
        let mut n = 0;
        if !q.is_empty() {
            n += 4 * usize::from(s.publisher().to_lowercase() == q);
            n += 3 * usize::from(s.short_name() == q);
            n += 2 * usize::from(s.title.to_lowercase() == q);
        }
        n += usize::from(s.repository.is_some()) + usize::from(s.website.is_some());
        n += usize::from(
            s.name.starts_with("io.github.")
                || s.name.starts_with("com.")
                || s.name.starts_with("io."),
        );
        n
    };
    list.sort_by_key(|s| std::cmp::Reverse(score(s)));
}

fn official_file() -> PathBuf {
    crate::paths::shared("cache")
        .join("extensions")
        .join("registry-official.json")
}

/// Official servers as last gathered, and whether that was within a day.
pub fn cached_official() -> Option<(Vec<RegistryServer>, bool)> {
    let file = official_file();
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .is_ok_and(|e| e < Duration::from_secs(24 * 3600));
    Some((crate::durable::load_json(file)?, fresh))
}

/// Blocking, a second or two: every server the known companies publish, one
/// registry search per namespace of theirs, a few dozen at a time.
pub fn official_servers() -> Result<Vec<RegistryServer>> {
    let namespaces = super::publishers::namespaces();
    let mut found: Vec<RegistryServer> = Vec::new();
    let mut failures = 0;
    for chunk in namespaces.chunks(32) {
        let pages: Vec<Result<(Vec<RegistryServer>, Option<String>)>> = std::thread::scope(|s| {
            chunk
                .iter()
                .map(|ns| s.spawn(move || fetch(ns, None)))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("panicked"))))
                .collect()
        });
        for page in pages {
            match page {
                Ok((list, _)) => found.extend(list.into_iter().filter(|s| s.official.is_some())),
                Err(_) => failures += 1,
            }
        }
    }
    if found.is_empty() && failures > 0 {
        bail!("Could not reach the MCP Registry");
    }
    found.sort_by(|a, b| {
        a.official
            .cmp(&b.official)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    found.dedup_by(|a, b| a.name == b.name);
    crate::durable::cache_json(&official_file(), &found);
    Ok(found)
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn parse_registry(v: &Value) -> (Vec<RegistryServer>, Option<String>) {
    let mut list = Vec::new();
    for e in v["servers"].as_array().into_iter().flatten() {
        let meta = &e["_meta"]["io.modelcontextprotocol.registry/official"];
        if meta["status"].as_str().is_some_and(|s| s != "active") {
            continue;
        }
        let s = &e["server"];
        let Some(name) = s["name"].as_str() else {
            continue;
        };
        let installs: Vec<Install> = s["remotes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(remote)
            .chain(
                s["packages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(package),
            )
            .collect();
        if installs.is_empty() {
            continue;
        }
        let text = |k: &str| s[k].as_str().filter(|t| !t.is_empty()).map(str::to_string);
        list.push(RegistryServer {
            name: name.to_string(),
            title: text("title")
                .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name).to_string()),
            description: text("description").unwrap_or_default(),
            version: text("version").unwrap_or_default(),
            website: text("websiteUrl"),
            repository: s["repository"]["url"]
                .as_str()
                .filter(|u| !u.is_empty())
                .map(str::to_string),
            installs,
            official: super::publishers::by_namespace(name).map(str::to_string),
        });
    }
    let next = v["metadata"]["nextCursor"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    (list, next)
}

fn input(v: &Value) -> Option<Input> {
    Some(Input {
        name: v["name"].as_str()?.to_string(),
        description: v["description"].as_str().unwrap_or_default().to_string(),
        required: v["isRequired"].as_bool().unwrap_or(false)
            || v["isSecret"].as_bool().unwrap_or(false),
        secret: v["isSecret"].as_bool().unwrap_or(false),
        default: v["default"]
            .as_str()
            .or(v["value"].as_str())
            .filter(|d| !d.contains('{'))
            .map(str::to_string),
    })
}

fn remote(r: &Value) -> Option<Install> {
    let transport = match r["type"].as_str()? {
        "streamable-http" | "http" => "http",
        "sse" => "sse",
        _ => return None,
    };
    let headers: Vec<Input> = r["headers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(input)
        .collect();
    // Some entries only work through a third party's key (Smithery's gateway).
    if headers
        .iter()
        .any(|h| h.description.to_lowercase().contains("smithery"))
        || r["url"].as_str()?.contains("smithery")
    {
        return None;
    }
    Some(Install::Remote {
        transport: transport.into(),
        url: r["url"].as_str()?.to_string(),
        headers,
    })
}

fn package(p: &Value) -> Option<Install> {
    if p["transport"]["type"].as_str().unwrap_or("stdio") != "stdio" {
        return None;
    }
    let id = p["identifier"].as_str()?;
    let version = p["version"]
        .as_str()
        .filter(|v| !v.is_empty() && *v != "latest");
    let mut placeholders = Vec::new();
    let mut values = |list: &Value| -> Vec<String> {
        let mut out = Vec::new();
        for a in list.as_array().into_iter().flatten() {
            for (var, spec) in a["variables"].as_object().into_iter().flatten() {
                let mut i = input(&json!({"name": var})).unwrap_or(Input {
                    name: var.clone(),
                    description: String::new(),
                    required: true,
                    secret: false,
                    default: None,
                });
                i.description = spec["description"].as_str().unwrap_or_default().to_string();
                i.secret = spec["isSecret"].as_bool().unwrap_or(false);
                i.required = true;
                if !placeholders.iter().any(|p: &Input| p.name == i.name) {
                    placeholders.push(i);
                }
            }
            let value = a["value"]
                .as_str()
                .or(a["default"].as_str())
                .unwrap_or_default();
            match a["type"].as_str() {
                Some("named") => {
                    out.push(a["name"].as_str().unwrap_or_default().to_string());
                    if !value.is_empty() {
                        out.push(value.to_string());
                    }
                }
                _ if !value.is_empty() => out.push(value.to_string()),
                _ => {}
            }
        }
        out
    };
    let runtime = values(&p["runtimeArguments"]);
    let package_args = values(&p["packageArguments"]);
    let env: Vec<Input> = p["environmentVariables"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(input)
        .collect();
    let (kind, command, mut args) = match p["registryType"].as_str()? {
        "npm" => {
            let spec = version.map_or(id.to_string(), |v| format!("{id}@{v}"));
            let mut a = runtime;
            if !a.iter().any(|x| x == "-y" || x == "--yes") {
                a.insert(0, "-y".into());
            }
            a.push(spec);
            ("npm", "npx", a)
        }
        "pypi" => {
            let mut a = runtime;
            a.push(version.map_or(id.to_string(), |v| format!("{id}=={v}")));
            ("pypi", "uvx", a)
        }
        "oci" => {
            let mut a = vec!["run".to_string(), "-i".into(), "--rm".into()];
            a.extend(runtime);
            // docker passes these through from the environment Claude gives it.
            for e in &env {
                if !a
                    .iter()
                    .any(|x| x.starts_with(&format!("{}=", e.name)) || *x == e.name)
                {
                    a.push("-e".into());
                    a.push(e.name.clone());
                }
            }
            a.push(id.to_string());
            ("docker", "docker", a)
        }
        _ => return None,
    };
    args.extend(package_args);
    Some(Install::Local {
        kind: kind.into(),
        command: command.into(),
        args,
        env,
        placeholders,
    })
}

/// The JSON `claude mcp add-json` takes for `install` with the user's `values`.
pub fn config(install: &Install, values: &HashMap<String, String>) -> Result<Value> {
    let missing: Vec<&str> = install
        .inputs()
        .into_iter()
        .filter(|i| i.required && values.get(&i.name).is_none_or(|v| v.trim().is_empty()))
        .map(|i| i.name.as_str())
        .collect();
    if !missing.is_empty() {
        bail!("Fill in {}", missing.join(", "));
    }
    let value = |i: &Input| {
        values
            .get(&i.name)
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .or_else(|| i.default.clone())
    };
    Ok(match install {
        Install::Remote {
            transport,
            url,
            headers,
        } => {
            let mut c = json!({"type": transport, "url": url});
            // A bare token in Authorization means a bearer one.
            let bearer = |name: &str, v: String| {
                let scheme = v.split_whitespace().count() > 1;
                if name.eq_ignore_ascii_case("authorization") && !scheme {
                    format!("Bearer {v}")
                } else {
                    v
                }
            };
            let h: serde_json::Map<String, Value> = headers
                .iter()
                .filter_map(|i| Some((i.name.clone(), Value::String(bearer(&i.name, value(i)?)))))
                .collect();
            if !h.is_empty() {
                c["headers"] = Value::Object(h);
            }
            c
        }
        Install::Local {
            command,
            args,
            env,
            placeholders,
            ..
        } => {
            let fill = |a: &String| {
                placeholders.iter().fold(a.clone(), |acc, p| {
                    acc.replace(&format!("{{{}}}", p.name), &value(p).unwrap_or_default())
                })
            };
            let mut c = json!({"type": "stdio", "command": command, "args": args.iter().map(fill).collect::<Vec<_>>()});
            let e: serde_json::Map<String, Value> = env
                .iter()
                .filter_map(|i| Some((i.name.clone(), Value::String(value(i)?))))
                .collect();
            if !e.is_empty() {
                c["env"] = Value::Object(e);
            }
            c
        }
    })
}

/// The command the config amounts to, for the user to read before installing (secrets masked).
/// `scope` is Claude Code's: `user`, `project` or `local`.
pub fn preview(name: &str, install: &Install, scope: &str) -> String {
    match install {
        Install::Remote {
            transport,
            url,
            headers,
        } => {
            let mut s = format!("claude mcp add --transport {transport} -s {scope} {name} {url}");
            for h in headers {
                s.push_str(&format!(
                    " -H \"{}: {}\"",
                    h.name,
                    if h.secret { "••••" } else { "…" }
                ));
            }
            s
        }
        Install::Local {
            command, args, env, ..
        } => {
            let mut s = format!("claude mcp add -s {scope} {name}");
            for e in env {
                s.push_str(&format!(
                    " -e {}={}",
                    e.name,
                    if e.secret { "••••" } else { "…" }
                ));
            }
            s.push_str(&format!(" -- {command} {}", args.join(" ")));
            s
        }
    }
}

/// Adds the server for your user; says whether it wants a sign-in next.
pub fn install_server(
    cwd: &Path,
    name: &str,
    install: &Install,
    values: &HashMap<String, String>,
) -> Result<bool> {
    install_server_as(cwd, name, install, values, "user")
}

/// Adds the server in Claude Code's `scope` (`user`, `project` or `local`);
/// says whether it wants a sign-in next.
pub fn install_server_as(
    cwd: &Path,
    name: &str,
    install: &Install,
    values: &HashMap<String, String>,
    scope: &str,
) -> Result<bool> {
    let config = config(install, values)?;
    let bin = launch::which("claude").ok_or_else(|| anyhow!("claude is not on PATH"))?;
    // On stdin, not in the arguments: the config can hold keys.
    super::claude_ext::run_cli_input(
        &bin,
        &scope_dir(cwd, scope),
        &["mcp", "add-json", name, "-", "-s", scope],
        config.to_string().as_bytes(),
        CLI_TIMEOUT,
    )?;
    Ok(matches!(install, Install::Remote { headers, .. } if headers.is_empty()))
}

fn run(bin: &Path, cwd: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    super::claude_ext::run_cli(bin, &workdir(cwd), args, timeout)
}

/// Everything here is for the user, not the project, so a folder that is gone
/// (an environment deleted outside arakun) must not stop it: use home instead.
fn workdir(cwd: &Path) -> PathBuf {
    if cwd.is_dir() {
        cwd.to_path_buf()
    } else {
        launch::home()
    }
}

// ---------------------------------------------------------------------------
// On a server: the app asks for the marketplace there and installs from it
// (`ClientMsg::Market`, `InstallPlugin` and `InstallServer`). Everything is
// for the server's user, so it runs in that user's home.

/// Blocking: the `market` message a server answers with. It holds the plugins
/// its marketplaces offer, each saying whether it is installed, and the names
/// of the MCP servers Claude has there. When the list cannot be read, it holds
/// the last one read and says why.
pub fn report() -> Value {
    let home = launch::home();
    let (plugins, error) = match plugins(&home) {
        Ok(list) => (list, None),
        Err(e) => (cached_plugins().unwrap_or_default(), Some(format!("{e:#}"))),
    };
    let servers: Vec<String> = super::claude_ext::load_quick(&home)
        .servers
        .into_iter()
        .map(|s| s.name)
        .collect();
    json!({ "type": "market", "plugins": plugins, "servers": servers, "error": error })
}

/// Blocking: installs a plugin on this machine, and says how it went (`market_done`).
pub fn serve_plugin(id: &str) -> Value {
    done(id, install_plugin(&launch::home(), id))
}

/// Blocking: adds an MCP server on this machine, and says how it went (`market_done`).
pub fn serve_server(name: &str, install: &Install, values: &HashMap<String, String>) -> Value {
    let result = install_server(&launch::home(), name, install, values).map(|_| ());
    done(name, result)
}

fn done(name: &str, result: Result<()>) -> Value {
    json!({
        "type": "market_done",
        "name": name,
        "error": result.err().map(|e| format!("{e:#}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_says_how_an_install_went() {
        let ok = done("a@b", Ok(()));
        assert_eq!(ok["type"], "market_done");
        assert_eq!(ok["name"], "a@b");
        assert!(ok["error"].is_null());
        let failed = done("a@b", Err(anyhow!("no such plugin")));
        assert_eq!(failed["error"], "no such plugin");
    }

    #[test]
    fn missing_folder_runs_from_home() {
        let here = std::env::temp_dir();
        assert_eq!(workdir(&here), here);
        assert_eq!(workdir(&here.join("arakun-no-such-folder")), launch::home());
    }

    fn sample() -> Value {
        json!({"servers": [
            {"server": {"name": "io.github.github/github-mcp-server", "title": "GitHub", "description": "GitHub's MCP", "version": "2.0.3",
              "remotes": [{"type": "streamable-http", "url": "https://api.githubcopilot.com/mcp/",
                "headers": [{"name": "Authorization", "isSecret": true, "description": "PAT"}]}],
              "packages": [{"registryType": "oci", "identifier": "ghcr.io/github/github-mcp-server:2.0.3", "transport": {"type": "stdio"},
                "runtimeArguments": [{"type": "named", "name": "-e", "value": "GITHUB_PERSONAL_ACCESS_TOKEN={token}",
                  "variables": {"token": {"isSecret": true}}}]}]},
             "_meta": {"io.modelcontextprotocol.registry/official": {"status": "active"}}},
            {"server": {"name": "com.pulsemcp/remote-filesystem", "version": "0.1.5",
              "packages": [{"registryType": "npm", "identifier": "remote-filesystem-mcp-server", "version": "0.1.5", "transport": {"type": "stdio"},
                "runtimeArguments": [{"type": "positional", "value": "-y"}],
                "environmentVariables": [{"name": "GCS_BUCKET", "isRequired": true}, {"name": "GCS_PRIVATE_KEY", "isSecret": true},
                  {"name": "GCS_MAKE_PUBLIC", "default": "false"}]}]},
             "_meta": {"io.modelcontextprotocol.registry/official": {"status": "active"}}},
            {"server": {"name": "ai.smithery/x", "remotes": [{"type": "streamable-http", "url": "https://server.smithery.ai/x/mcp"}]},
             "_meta": {"io.modelcontextprotocol.registry/official": {"status": "active"}}},
            {"server": {"name": "old/one", "remotes": [{"type": "sse", "url": "https://x"}]},
             "_meta": {"io.modelcontextprotocol.registry/official": {"status": "deleted"}}}
        ], "metadata": {"nextCursor": "c1", "count": 4}})
    }

    #[test]
    fn reads_registry() {
        let (list, next) = parse_registry(&sample());
        assert_eq!(next.as_deref(), Some("c1"));
        assert_eq!(list.len(), 2);
        let gh = &list[0];
        assert_eq!(
            (
                gh.short_name().as_str(),
                gh.publisher().as_str(),
                gh.installs.len()
            ),
            ("github", "github", 2)
        );
        let Install::Local {
            command,
            args,
            placeholders,
            ..
        } = &gh.installs[1]
        else {
            panic!()
        };
        assert_eq!(command, "docker");
        assert_eq!(
            args,
            &[
                "run",
                "-i",
                "--rm",
                "-e",
                "GITHUB_PERSONAL_ACCESS_TOKEN={token}",
                "ghcr.io/github/github-mcp-server:2.0.3"
            ]
        );
        assert!(placeholders[0].secret);
        let mut values = HashMap::new();
        values.insert("token".to_string(), "ghp_x".to_string());
        let c = config(&gh.installs[1], &values).unwrap();
        assert_eq!(c["args"][4], "GITHUB_PERSONAL_ACCESS_TOKEN=ghp_x");
        let mut values = HashMap::new();
        values.insert("Authorization".to_string(), "ghp_x".to_string());
        assert_eq!(
            config(&gh.installs[0], &values).unwrap()["headers"]["Authorization"],
            "Bearer ghp_x"
        );

        let fs = &list[1];
        assert_eq!(fs.short_name(), "remote-filesystem");
        let Install::Local {
            command, args, env, ..
        } = &fs.installs[0]
        else {
            panic!()
        };
        assert_eq!(
            (command.as_str(), args.clone()),
            (
                "npx",
                vec![
                    "-y".to_string(),
                    "remote-filesystem-mcp-server@0.1.5".to_string()
                ]
            )
        );
        assert_eq!(env.iter().filter(|e| e.required).count(), 2);
        assert!(
            config(&fs.installs[0], &HashMap::new())
                .unwrap_err()
                .to_string()
                .contains("GCS_BUCKET")
        );
        let mut values = HashMap::new();
        values.insert("GCS_BUCKET".into(), "b".into());
        values.insert("GCS_PRIVATE_KEY".into(), "k".into());
        let c = config(&fs.installs[0], &values).unwrap();
        assert_eq!(
            c["env"],
            json!({"GCS_BUCKET": "b", "GCS_PRIVATE_KEY": "k", "GCS_MAKE_PUBLIC": "false"})
        );
        let shown = preview("remote-filesystem", &fs.installs[0], "local");
        assert!(shown.contains("-e GCS_PRIVATE_KEY=••••"));
        // It shows the scope the install uses.
        assert!(shown.contains("-s local"));
    }

    #[test]
    fn plugin_publishers() {
        let p = |v: Value, name: &str| publisher(&v, name);
        assert_eq!(
            p(
                json!({"source": "./plugins/frontend-design", "author": {"name": "Anthropic"}}),
                "frontend-design"
            ),
            (Some("Anthropic".into()), true)
        );
        assert_eq!(
            p(json!({"source": "./external_plugins/github"}), "github"),
            (Some("GitHub".into()), true)
        );
        assert_eq!(
            p(
                json!({"source": {"source": "url", "url": "https://github.com/stripe/ai.git"}}),
                "stripe"
            ),
            (Some("Stripe".into()), true)
        );
        assert_eq!(
            p(
                json!({"source": {"url": "https://github.com/42Crunch-AI/claude-plugins.git"}, "author": {"name": "42Crunch"}}),
                "x"
            ),
            (Some("42Crunch".into()), true)
        );
        assert_eq!(
            p(
                json!({"source": {"url": "https://github.com/supabase-community/supabase-plugin.git"}}),
                "supabase"
            ),
            (Some("Supabase".into()), true)
        );
        assert_eq!(
            p(
                json!({"source": {"url": "https://github.com/someone-community/x.git"}, "author": {"name": "Someone"}}),
                "x"
            ),
            (Some("Someone".into()), false)
        );
        assert_eq!(
            p(
                json!({"source": {"url": "https://github.com/obra/superpowers.git"}}),
                "superpowers"
            ),
            (Some("obra".into()), false)
        );
        assert_eq!(
            p(
                json!({"source": "x", "author": {"name": "Google LLC"}}),
                "x"
            ),
            (Some("Google".into()), true)
        );
    }

    #[test]
    fn reads_plugins() {
        let v = json!({"installed": [{"id": "posthog@official"}], "available": [
            {"pluginId": "a@official", "name": "a", "description": "A", "marketplaceName": "official", "installCount": 5},
            {"pluginId": "posthog@official", "name": "posthog", "marketplaceName": "official", "installCount": 50}]});
        let mut m = HashMap::new();
        m.insert(
            "a@official".to_string(),
            Manifest {
                category: Some("database".to_string()),
                ..Default::default()
            },
        );
        let list = parse_plugins(&v, &m);
        assert_eq!(list[0].name, "posthog");
        assert!(list[0].installed);
        assert_eq!(list[1].category.as_deref(), Some("database"));
        assert_eq!(short_count(1_266_133), "1.3M");
        assert_eq!(short_count(4_000), "4k");
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "reaches the MCP Registry"]
    fn search_live() {
        let (list, next) = super::search("github", None).unwrap();
        for s in list.iter().take(8) {
            eprintln!(
                "{} | {} | {} | {:?}",
                s.name,
                s.short_name(),
                s.publisher(),
                s.installs
                    .iter()
                    .map(|i| i.kind().to_string())
                    .collect::<Vec<_>>()
            );
        }
        eprintln!("{} results, next {:?}", list.len(), next);
    }
}

#[cfg(test)]
mod live_official {
    #[test]
    #[ignore = "reaches the MCP Registry and reads the local marketplaces"]
    fn official_live() {
        let t = std::time::Instant::now();
        let list = super::official_servers().unwrap();
        eprintln!("{} official servers in {:?}", list.len(), t.elapsed());
        let mut by: std::collections::BTreeMap<String, usize> = Default::default();
        for s in &list {
            *by.entry(s.official.clone().unwrap()).or_default() += 1;
        }
        eprintln!("{by:?}");
        let v: serde_json::Value = serde_json::json!({"available": []});
        let _ = v;
        let m = super::manifests();
        let off: Vec<_> = m
            .iter()
            .filter(|(_, x)| x.official)
            .map(|(k, x)| format!("{k}={}", x.publisher.clone().unwrap_or_default()))
            .collect();
        eprintln!(
            "{} of {} plugins official; sample {:?}",
            off.len(),
            m.len(),
            &off[..off.len().min(25)]
        );
        let not: Vec<_> = m
            .iter()
            .filter(|(_, x)| !x.official)
            .map(|(k, x)| format!("{k}={}", x.publisher.clone().unwrap_or_default()))
            .collect();
        eprintln!("not official sample {:?}", &not[..not.len().min(40)]);
    }
}
