//! `arakun doctor [--json]`: what Arakun and its agents need on this computer,
//! one line per check with the command that fixes it. The setup finds the
//! agents and `gh` the same way (`onboarding`); this adds their sign-ins and
//! the tools and system parts the README lists under Requirements. Works
//! without the app, and reads no secret: only whether a sign-in is there.

use serde::Serialize;

use crate::agent::AgentKind;
use crate::onboarding::{self, GitHub, Readiness};

const USAGE: &str = "\
usage: arakun doctor [--json]

Checks what Arakun and its agents need on this computer: the agents and their
sign-ins, Node.js and npm, git, the GitHub CLI, the browser tabs, the sandbox and
the graphics driver. Prints one line per check, and the command that fixes what
is missing. --json prints the same as JSON. Exits 1 when no agent can run a
session, else 0.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    /// Works, but less: a missing sign-in or optional tool.
    Warn,
    /// Arakun can't run a session.
    Fail,
    /// Not installed, and not needed: an agent the user may not want.
    Off,
}

impl Status {
    fn mark(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Off => "-",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Check {
    name: String,
    status: Status,
    detail: String,
    /// The command, or the page, that fixes it.
    fix: Option<String>,
}

fn check(name: &str, status: Status, detail: &str, fix: Option<&str>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
        fix: fix.map(Into::into),
    }
}

pub fn cli(args: &[String]) -> i32 {
    let json = match args {
        [] => false,
        [a] if a == "--json" => true,
        [a] if crate::control::is_help(a) => {
            println!("{USAGE}");
            return 0;
        }
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    // What the app would see, started from the desktop.
    crate::platform::import_login_path();
    let checks = run();
    if json {
        let out = serde_json::json!({ "ok": ok(&checks), "checks": checks });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        print!("{}", text(&checks));
    }
    if ok(&checks) { 0 } else { 1 }
}

/// Every check, in the order they print.
fn run() -> Vec<Check> {
    // It asks GitHub, so it runs while the rest look on disk.
    let github = std::thread::spawn(onboarding::find_github);
    let mut checks = agents();
    checks.push(node());
    checks.push(tool(
        "git",
        "git",
        "changes, environments and branches",
        "install git: https://git-scm.com/downloads",
    ));
    checks.push(gh(github.join().unwrap_or(GitHub::NoGh)));
    #[cfg(target_os = "linux")]
    checks.push(tool(
        "bwrap",
        "bwrap",
        "sandboxed and headless agents",
        "install bubblewrap (sudo apt install bubblewrap, or your system's package)",
    ));
    checks.push(browser(crate::browser::health()));
    #[cfg(target_os = "linux")]
    {
        checks.push(display(crate::window_error::check_display()));
        checks.push(vulkan(has_vulkan_driver()));
    }
    checks
}

/// Arakun can run a session when nothing failed.
fn ok(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.status != Status::Fail)
}

// ---------------------------------------------------------------------------
// Agents

/// One row per agent, and a failed one when none of them can run.
fn agents() -> Vec<Check> {
    let mut checks: Vec<Check> = onboarding::find_agents()
        .into_iter()
        .map(|row| agent(row.agent, row.state, signed_in(row.agent)))
        .collect();
    checks.push(workers_ai(crate::agent::workers_ai::credentials().is_ok()));
    if let Some(none) = no_agent(&checks) {
        checks.push(none);
    }
    checks
}

/// Whether the agent has a sign-in Arakun can see; `None` when it can't tell.
fn signed_in(agent: AgentKind) -> Option<bool> {
    use crate::agent as a;
    match agent {
        AgentKind::Claude => Some(a::plan::has_sign_in() || a::claude_auth::access().is_some()),
        AgentKind::Codex => Some(a::codex_sign_in().is_some() || a::codex_env_key().is_some()),
        AgentKind::OpenCode => Some(!a::opencode_providers().is_empty()),
        AgentKind::Devin => Some(a::devin_signed_in() || a::devin_env_key().is_some()),
        AgentKind::Cursor => Some(a::cursor_signed_in() || a::cursor_env_key().is_some()),
        // agy keeps its sign-in in the system keyring when there is one,
        // which Arakun does not read: no file is no answer.
        AgentKind::Antigravity => {
            (a::antigravity_signed_in() || a::antigravity_env_key().is_some()).then_some(true)
        }
        // droid's login is its own business; plain `droid` asks for it.
        AgentKind::Droid | AgentKind::WorkersAi => None,
    }
}

/// The command that signs the agent in.
fn sign_in_command(agent: AgentKind) -> Option<&'static str> {
    Some(match agent {
        AgentKind::Claude => "run `claude` and sign in with /login, or set ANTHROPIC_API_KEY",
        AgentKind::Codex => "codex login, or set CODEX_API_KEY or OPENAI_API_KEY",
        AgentKind::OpenCode => "opencode auth login",
        AgentKind::Devin => "devin auth login",
        AgentKind::Cursor => "cursor-agent login",
        AgentKind::Antigravity => "agy, then sign in with Google",
        AgentKind::Droid => "droid, then sign in",
        AgentKind::WorkersAi => return None,
    })
}

fn agent(agent: AgentKind, state: Readiness, signed_in: Option<bool>) -> Check {
    let name = agent.label();
    let (installed, first_use) = match state {
        Readiness::Ready => ("installed", false),
        Readiness::OnFirstUse => ("the first session installs it with npm", true),
        Readiness::Missing => {
            let (what, url) = onboarding::install_link(agent);
            let fix = format!("{what}: {url}");
            return check(name, Status::Off, "not installed", Some(&fix));
        }
    };
    match signed_in {
        // Neither installed nor signed in: an agent the user may not want.
        Some(false) if first_use => check(
            name,
            Status::Off,
            &format!("{installed}; no sign-in found"),
            sign_in_command(agent),
        ),
        Some(false) => check(
            name,
            Status::Warn,
            &format!("{installed}; no sign-in found"),
            sign_in_command(agent),
        ),
        Some(true) => check(name, Status::Ok, &format!("{installed}, signed in"), None),
        None if first_use => check(name, Status::Ok, installed, None),
        None => check(
            name,
            Status::Ok,
            &format!("{installed}; sign-in not checked"),
            None,
        ),
    }
}

/// Workers AI runs in Arakun itself, on a Cloudflare token.
fn workers_ai(has_token: bool) -> Check {
    let name = AgentKind::WorkersAi.label();
    if has_token {
        return check(name, Status::Ok, "Cloudflare API token found", None);
    }
    check(
        name,
        Status::Off,
        "no Cloudflare API token",
        Some(
            "set CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID, or opencode auth login and pick \
             Cloudflare Workers AI",
        ),
    )
}

/// The failed row when no agent can run a session.
fn no_agent(agents: &[Check]) -> Option<Check> {
    agents.iter().all(|c| c.status == Status::Off).then(|| {
        check(
            "Agents",
            Status::Fail,
            "no agent can run a session",
            Some("install one of the agents above and sign it in"),
        )
    })
}

// ---------------------------------------------------------------------------
// Tools

fn node() -> Check {
    if crate::agent::which("npm").is_some() {
        return check("Node.js", Status::Ok, "npm found", None);
    }
    check(
        "Node.js",
        Status::Warn,
        "no npm: Claude Code and Codex need it",
        Some("install Node.js with npm: https://nodejs.org"),
    )
}

/// A program on the `PATH` that some features need.
fn tool(name: &str, program: &str, for_what: &str, fix: &str) -> Check {
    tool_check(name, crate::agent::which(program).is_some(), for_what, fix)
}

fn tool_check(name: &str, found: bool, for_what: &str, fix: &str) -> Check {
    if found {
        return check(name, Status::Ok, "found", None);
    }
    check(
        name,
        Status::Warn,
        &format!("not found; needed for {for_what}"),
        Some(fix),
    )
}

fn gh(state: GitHub) -> Check {
    let name = "GitHub CLI";
    match state {
        GitHub::Connected => check(name, Status::Ok, "gh is signed in", None),
        GitHub::SignedOut => check(
            name,
            Status::Warn,
            "gh is not signed in (or GitHub did not answer)",
            Some(onboarding::GH_LOGIN),
        ),
        GitHub::NoGh => check(
            name,
            Status::Warn,
            "no gh; needed for CI, pull requests and the GitHub tab",
            Some(&format!(
                "install it from {}, then {}",
                onboarding::GH_URL,
                onboarding::GH_LOGIN
            )),
        ),
    }
}

// ---------------------------------------------------------------------------
// System

const APPARMOR_DOC: &str =
    "https://github.com/ArakunHQ/arakun/blob/main/docs/browser.md#browser-tabs-on-ubuntu";

fn browser(health: crate::browser::Health) -> Check {
    use crate::browser::Health;
    let name = "Browser tabs";
    match health {
        Health::Ready => check(name, Status::Ok, "ready", None),
        Health::NotBuilt => check(name, Status::Off, "this build has no browser tabs", None),
        Health::NoRuntime(path) => {
            let fix = if crate::update::release_build() {
                if cfg!(windows) {
                    "install Arakun again: irm https://arakun.dev/install.ps1 | iex"
                } else {
                    "install Arakun again: curl -fsSL https://arakun.dev/install | sh"
                }
            } else {
                "tools/install-cef.sh"
            };
            check(
                name,
                Status::Warn,
                &format!("no browser runtime at {}", path.display()),
                Some(fix),
            )
        }
        Health::SandboxBlocked => check(
            name,
            Status::Warn,
            "AppArmor blocks Chromium's sandbox; arakun needs a profile",
            Some(&format!("add the profile in {APPARMOR_DOC}")),
        ),
    }
}

#[cfg(target_os = "linux")]
fn display(found: Result<(), String>) -> Check {
    match found {
        Ok(()) => check("Desktop", Status::Ok, "Wayland or X11 answers", None),
        Err(e) => check(
            "Desktop",
            Status::Warn,
            &format!("{e}; the window can't open here"),
            Some("run it on your desktop, or arakun server serve on a machine without one"),
        ),
    }
}

#[cfg(target_os = "linux")]
fn vulkan(found: bool) -> Check {
    if found {
        return check("Vulkan", Status::Ok, "a driver is installed", None);
    }
    check(
        "Vulkan",
        Status::Warn,
        "no Vulkan driver found; the window may not open",
        Some(
            "install mesa-vulkan-drivers (Debian, Ubuntu, Fedora), vulkan-radeon, vulkan-intel \
             or vulkan-swrast (Arch), or the NVIDIA driver",
        ),
    )
}

/// Whether a Vulkan driver is installed: a manifest where the Vulkan loader
/// looks for one, or a variable that names one.
#[cfg(target_os = "linux")]
fn has_vulkan_driver() -> bool {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if var("VK_DRIVER_FILES").is_some() || var("VK_ICD_FILENAMES").is_some() {
        return true;
    }
    let data_dirs = var("XDG_DATA_DIRS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut dirs: Vec<std::path::PathBuf> = vec![
        crate::platform::data_home().join("vulkan/icd.d"),
        "/etc/vulkan/icd.d".into(),
    ];
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| std::path::Path::new(d).join("vulkan/icd.d")),
    );
    dirs.iter().any(|d| has_manifest(d))
}

#[cfg(target_os = "linux")]
fn has_manifest(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "json"))
    })
}

// ---------------------------------------------------------------------------
// Output

/// One line per check, the fix below it, and a last line that sums it up.
fn text(checks: &[Check]) -> String {
    let wide = checks
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for c in checks {
        out.push_str(&format!(
            "{:<5} {:<wide$}  {}\n",
            c.status.mark(),
            c.name,
            c.detail
        ));
        if let Some(fix) = &c.fix {
            out.push_str(&format!("{:<5} {:<wide$}  fix: {fix}\n", "", ""));
        }
    }
    let warnings = checks.iter().filter(|c| c.status == Status::Warn).count();
    out.push('\n');
    out.push_str(&match (ok(checks), warnings) {
        (false, _) => "Arakun can't run a session yet: fix what says FAIL.\n".to_string(),
        (true, 0) => "Everything Arakun needs is here.\n".to_string(),
        (true, 1) => "Arakun can run sessions; 1 thing could be better.\n".to_string(),
        (true, n) => format!("Arakun can run sessions; {n} things could be better.\n"),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_says_whether_it_is_signed_in() {
        let ready = agent(AgentKind::Cursor, Readiness::Ready, Some(true));
        assert_eq!(ready.status, Status::Ok);
        assert_eq!(ready.detail, "installed, signed in");
        assert_eq!(ready.fix, None);

        let out = agent(AgentKind::Cursor, Readiness::Ready, Some(false));
        assert_eq!(out.status, Status::Warn);
        assert_eq!(out.fix.as_deref(), Some("cursor-agent login"));

        let droid = agent(AgentKind::Droid, Readiness::Ready, None);
        assert_eq!(droid.status, Status::Ok);
        assert!(droid.detail.contains("not checked"));

        let npm = agent(AgentKind::Claude, Readiness::OnFirstUse, Some(true));
        assert_eq!(npm.status, Status::Ok);
        assert!(npm.detail.contains("npm"));
        // An adapter not installed and not signed in is an agent not in use.
        let unused = agent(AgentKind::Codex, Readiness::OnFirstUse, Some(false));
        assert_eq!(unused.status, Status::Off);
        assert!(unused.fix.unwrap().starts_with("codex login"));

        let missing = agent(AgentKind::OpenCode, Readiness::Missing, Some(false));
        assert_eq!(missing.status, Status::Off);
        assert!(missing.fix.unwrap().contains("https://"));
    }

    #[test]
    fn it_fails_only_when_no_agent_can_run() {
        let off = vec![
            agent(AgentKind::Claude, Readiness::Missing, None),
            workers_ai(false),
        ];
        let none = no_agent(&off).unwrap();
        assert_eq!(none.status, Status::Fail);

        // A missing sign-in is a warning: the agent still starts and asks.
        let one = vec![
            agent(AgentKind::Claude, Readiness::Ready, Some(false)),
            workers_ai(false),
        ];
        assert!(no_agent(&one).is_none());
        assert!(ok(&one));

        let mut all = off.clone();
        all.push(none);
        assert!(!ok(&all));
    }

    #[test]
    fn gh_says_how_to_sign_in() {
        assert_eq!(gh(GitHub::Connected).status, Status::Ok);
        assert_eq!(gh(GitHub::SignedOut).fix.as_deref(), Some("gh auth login"));
        let none = gh(GitHub::NoGh);
        assert!(none.fix.unwrap().contains("https://cli.github.com"));
    }

    #[test]
    fn the_browser_check_points_at_the_apparmor_profile() {
        use crate::browser::Health;
        let blocked = browser(Health::SandboxBlocked);
        assert_eq!(blocked.status, Status::Warn);
        assert!(blocked.fix.unwrap().contains("#browser-tabs-on-ubuntu"));
        assert_eq!(browser(Health::NotBuilt).status, Status::Off);
        let missing = browser(Health::NoRuntime("/x/libcef.so".into()));
        assert!(missing.detail.contains("/x/libcef.so"));
        assert!(missing.fix.is_some());
    }

    #[test]
    fn a_missing_tool_names_what_needs_it() {
        let found = tool_check("git", true, "branches", "install git");
        assert_eq!(found.status, Status::Ok);
        let missing = tool_check("git", false, "branches", "install git");
        assert_eq!(missing.status, Status::Warn);
        assert_eq!(missing.detail, "not found; needed for branches");
    }

    #[test]
    fn the_text_lines_up_and_sums_up() {
        let checks = vec![
            check("git", Status::Ok, "found", None),
            check(
                "GitHub CLI",
                Status::Warn,
                "not signed in",
                Some("gh auth login"),
            ),
        ];
        assert_eq!(
            text(&checks),
            "ok    git         found\n\
             warn  GitHub CLI  not signed in\n\
             \x20                 fix: gh auth login\n\
             \n\
             Arakun can run sessions; 1 thing could be better.\n"
        );
        let failed = vec![check("Agents", Status::Fail, "none", None)];
        assert!(text(&failed).ends_with("fix what says FAIL.\n"));
    }

    #[test]
    fn json_names_the_status_in_lower_case() {
        let v = serde_json::to_value(check("git", Status::Warn, "x", Some("y"))).unwrap();
        assert_eq!(v["status"], "warn");
        assert_eq!(v["fix"], "y");
        let v = serde_json::to_value(check("git", Status::Ok, "x", None)).unwrap();
        assert!(v["fix"].is_null());
    }

    #[test]
    fn the_usage_line_matches_the_command_list() {
        assert_eq!(
            crate::cli::usage("doctor").as_deref(),
            Some("arakun doctor [--json]")
        );
        assert!(USAGE.starts_with("usage: arakun doctor [--json]\n"));
    }
}
