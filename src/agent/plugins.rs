//! Agent plugins arakun knows about, Claude SEO above all: what is installed
//! for Droid and Claude Code, whether its Python runtime is ready, and the
//! script that installs or updates it. Reads files and local CLIs only.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::launch;

/// Droid names a marketplace after its repository, Claude Code after its manifest.
pub const SEO_DROID: &str = "claude-seo@claude-seo";
pub const SEO_CLAUDE: &str = "claude-seo@agricidaniel-claude-seo";

#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub path: PathBuf,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Runtime {
    pub ready: bool,
    pub browser: bool,
    pub version: String,
}

#[derive(Clone, Debug, Default)]
pub struct Plugins {
    /// Every Droid plugin, sorted by id.
    pub droid: Vec<Installed>,
    pub claude_cli: bool,
    pub python: bool,
    pub seo_droid: Option<(Installed, Option<Runtime>)>,
    pub seo_claude: Option<(Installed, Option<Runtime>)>,
}

/// Droid records git installs by full commit hash.
fn short(version: &str) -> String {
    let hex = version.len() >= 12 && version.chars().all(|c| c.is_ascii_hexdigit());
    if hex { version[..7].to_string() } else { version.to_string() }
}

fn droid_record(record: &Value, settings: &Value) -> Option<Installed> {
    let id = record["pluginId"].as_str()?.to_string();
    let entry = &record["entry"];
    let path = PathBuf::from(entry["installPath"].as_str()?);
    let enabled = settings["enabledPlugins"][&id].as_bool().unwrap_or(true);
    Some(Installed { version: short(entry["version"].as_str().unwrap_or_default()), id, path, enabled })
}

fn claude_list(list: &Value) -> Vec<Installed> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Installed {
                id: p["id"].as_str()?.to_string(),
                version: short(p["version"].as_str().unwrap_or_default()),
                path: PathBuf::from(p["installPath"].as_str().unwrap_or_default()),
                enabled: p["enabled"].as_bool().unwrap_or(true),
            })
        })
        .collect()
}

fn doctor(v: &Value) -> Option<Runtime> {
    Some(Runtime {
        ready: v.get("ready")?.as_bool()?,
        browser: v["browser_ready"].as_bool().unwrap_or(false),
        version: v["plugin_version"].as_str().unwrap_or_default().to_string(),
    })
}

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

fn droid_installed(factory: &Path) -> Vec<Installed> {
    let settings = read_json(&factory.join("settings.json"));
    let mut all: Vec<Installed> = std::fs::read_dir(factory.join("plugins/installed_plugins"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| droid_record(&read_json(&e.path()), &settings))
        .collect();
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all
}

fn json_output(cmd: &mut Command) -> Value {
    cmd.env("PATH", launch::child_path_env())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or(Value::Null)
}

/// `doctor` exits 3 when the runtime needs setup; its JSON is what matters.
fn runtime(root: &Path, data: Option<&Path>) -> Option<Runtime> {
    let mut cmd = crate::platform::command(root.join("scripts/claude-seo"));
    cmd.args(["doctor", "--json"]);
    if let Some(d) = data {
        cmd.env("CLAUDE_PLUGIN_DATA", d);
    }
    doctor(&json_output(&mut cmd))
}

/// Where Claude Code keeps the plugin's data, its runtime included.
fn claude_seo_data() -> PathBuf {
    launch::home().join(".claude/plugins/data/claude-seo-agricidaniel-claude-seo")
}

/// Blocking (runs `claude` and the runtime's `doctor`): call it off the main thread.
pub fn load() -> Plugins {
    let droid = droid_installed(&launch::home().join(".factory"));
    let claude_cli = launch::which("claude").is_some();
    let python = launch::which("python3").is_some();
    let seo_droid = droid.iter().find(|p| p.id == SEO_DROID).cloned().map(|p| {
        let r = runtime(&p.path, None);
        (p, r)
    });
    let seo_claude = if claude_cli { claude_list(&json_output(crate::platform::command(launch::which("claude").unwrap_or_else(|| "claude".into())).args(["plugin", "list", "--json"]))) } else { Default::default() }
        .into_iter()
        .find(|p| p.id == SEO_CLAUDE)
        .map(|p| {
            let r = runtime(&p.path, Some(&claude_seo_data()));
            (p, r)
        });
    Plugins { droid, claude_cli, python, seo_droid, seo_claude }
}

// Both CLIs exit 0 when an install fails, so each step is checked with `list`.
const DROID_STEPS: &str = r#"set -e
# setup rebuilds the environment every time; skip it while doctor is happy.
seo_setup() {
  state=$("$1/scripts/claude-seo" doctor --json 2>/dev/null || true)
  if echo "$state" | grep -q '"ready": true' && echo "$state" | grep -q '"browser_ready": true'; then
    echo 'Python runtime and Chromium already set up.'
  else
    "$1/scripts/claude-seo" setup
  fi
}
echo '== Claude SEO for Droid'
droid plugin marketplace list | grep -q 'AgriciDaniel/claude-seo' || droid plugin marketplace add https://github.com/AgriciDaniel/claude-seo
droid plugin marketplace update claude-seo
if droid plugin list --scope user | grep -q 'claude-seo@claude-seo'; then
  droid plugin update claude-seo@claude-seo --scope user
else
  droid plugin install claude-seo@claude-seo --scope user
fi
root=$(python3 -c 'import glob,json,os
for f in glob.glob(os.path.expanduser("~/.factory/plugins/installed_plugins/*.json")):
    r = json.load(open(f))
    if r.get("pluginId") == "claude-seo@claude-seo": print(r["entry"]["installPath"])')
[ -n "$root" ] || { echo 'Droid did not install claude-seo.'; exit 1; }
seo_setup "$root"
"#;

const CLAUDE_STEPS: &str = r#"echo
echo '== Claude SEO for Claude Code'
claude plugin marketplace list | grep -q 'agricidaniel-claude-seo' || claude plugin marketplace add AgriciDaniel/claude-seo
claude plugin marketplace update agricidaniel-claude-seo
if claude plugin list --json | grep -q '"claude-seo@agricidaniel-claude-seo"'; then
  claude plugin update claude-seo@agricidaniel-claude-seo
else
  claude plugin install claude-seo@agricidaniel-claude-seo
fi
root=$(claude plugin list --json | python3 -c 'import json,sys
for p in json.load(sys.stdin):
    if p["id"] == "claude-seo@agricidaniel-claude-seo": print(p["installPath"])')
[ -n "$root" ] || { echo 'Claude Code did not install claude-seo.'; exit 1; }
mkdir -p "$HOME/.claude/plugins/data/claude-seo-agricidaniel-claude-seo"
CLAUDE_PLUGIN_DATA="$HOME/.claude/plugins/data/claude-seo-agricidaniel-claude-seo" seo_setup "$root"
"#;

pub fn seo_script(claude: bool) -> String {
    let mut s = DROID_STEPS.to_string();
    if claude {
        s.push_str(CLAUDE_STEPS);
    }
    s.push_str("echo\necho 'Claude SEO is ready. Start a new session and try: /seo audit https://example.com'\n");
    s
}

/// Writes the script into `dir` and returns the command that runs it, so the
/// terminal tab shows a short command instead of the whole script.
pub fn write_seo_script(dir: &Path, claude: bool) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("claude-seo-install.sh");
    std::fs::write(&path, seo_script(claude))?;
    Ok(format!("sh '{}'", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_droid_record() {
        let record = json!({"pluginId": "claude-seo@claude-seo", "entry": {
            "installPath": "/c/4b99", "version": "4b99de2f7de7", "scope": "user"}});
        let enabled = json!({"enabledPlugins": {"claude-seo@claude-seo": false}});
        let p = droid_record(&record, &enabled).unwrap();
        assert_eq!(p.id, SEO_DROID);
        assert_eq!(p.version, "4b99de2");
        assert_eq!(p.path, PathBuf::from("/c/4b99"));
        assert!(!p.enabled);
        // Missing from enabledPlugins means enabled, as Droid treats it.
        assert!(droid_record(&record, &json!({})).unwrap().enabled);
        assert!(droid_record(&json!({"pluginId": "x"}), &json!({})).is_none());
    }

    #[test]
    fn reads_claude_list() {
        let list = json!([
            {"id": "posthog@claude-plugins-official", "version": "1.1.67", "enabled": true, "installPath": "/p"},
            {"id": "claude-seo@agricidaniel-claude-seo", "version": "2.4.2", "enabled": true, "installPath": "/s"}
        ]);
        let all = claude_list(&list);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].version, "2.4.2");
        assert!(claude_list(&json!({"error": "x"})).is_empty());
    }

    #[test]
    fn reads_doctor() {
        let r = doctor(&json!({"ready": true, "browser_ready": false, "plugin_version": "2.4.2", "reasons": []}))
            .unwrap();
        assert!(r.ready && !r.browser);
        assert_eq!(r.version, "2.4.2");
        assert!(doctor(&json!("nope")).is_none());
    }

    #[test]
    fn loads_from_a_factory_home() {
        let dir = std::env::temp_dir().join(format!("arakun-plugins-{}", std::process::id()));
        let records = dir.join("plugins/installed_plugins");
        std::fs::create_dir_all(&records).unwrap();
        std::fs::write(
            records.join("a.json"),
            r#"{"pluginId":"claude-seo@claude-seo","entry":{"installPath":"/nowhere","version":"2.4.2"}}"#,
        )
        .unwrap();
        std::fs::write(records.join("broken.json"), "{").unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"enabledPlugins":{}}"#).unwrap();
        let found = droid_installed(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, SEO_DROID);
    }

    #[test]
    fn script_verifies_and_sets_up() {
        let s = seo_script(true);
        assert!(s.starts_with("set -e\n"));
        assert!(s.contains("droid plugin marketplace add https://github.com/AgriciDaniel/claude-seo"));
        assert!(s.contains("Droid did not install claude-seo"));
        assert!(s.contains("claude plugin install claude-seo@agricidaniel-claude-seo"));
        assert!(s.contains("CLAUDE_PLUGIN_DATA="));
        assert_eq!(s.matches("seo_setup \"$root\"").count(), 2);
        assert!(!seo_script(false).contains("claude plugin"));
    }

    /// Needs both CLIs: `cargo test live_plugins -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_plugins() {
        let p = load();
        eprintln!("{p:#?}");
        let dir = std::env::temp_dir().join("arakun-plugins-live");
        eprintln!("{}", write_seo_script(&dir, p.claude_cli).unwrap());
    }
}
