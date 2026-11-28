//! The GitHub host `gh` reads, and whether `gh` is ready.

/// The host `gh` reads `-R owner/name` on, chosen as `gh` chooses it:
/// `GH_HOST` when set, else the only host `gh` is signed in to, else GitHub.
/// Signed in to GitHub Enterprise alone, `gh` reads there with no `GH_HOST`.
///
/// It never waits on the disk: the palette asks while it draws. `gh`'s
/// sign-ins are read on another thread and kept, and read again when the
/// kept answer is old, so signing in to a new host shows up a moment later.
pub fn gh_host() -> String {
    env_host().unwrap_or_else(signed_in_host)
}

/// `gh_host` read now, for the command line, which asks once and exits
/// before a read on another thread would be back.
pub(super) fn gh_host_now() -> String {
    env_host().unwrap_or_else(|| default_host(None, read_hosts_yml().as_deref()))
}

fn env_host() -> Option<String> {
    std::env::var("GH_HOST")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// The host from `gh`'s sign-ins as last read, GitHub until a read is back.
fn signed_in_host() -> String {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    // The answer, when it was asked for, and whether a read is running.
    static KEPT: Mutex<(Option<String>, Option<Instant>, bool)> = Mutex::new((None, None, false));
    const FRESH: Duration = Duration::from_secs(30);
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    let stale = kept.1.is_none_or(|at| at.elapsed() > FRESH);
    if stale && !kept.2 {
        kept.1 = Some(Instant::now());
        kept.2 = true;
        std::thread::spawn(|| {
            let host = default_host(None, read_hosts_yml().as_deref());
            let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
            kept.0 = Some(host);
            kept.2 = false;
        });
    }
    kept.0.clone().unwrap_or_else(|| "github.com".into())
}

/// Whether `gh` looks able to read: installed, and signed in to a host or
/// given a token. For a click, not while drawing: the first call runs
/// `gh --version`, and every call reads `gh`'s small sign-in file.
pub fn gh_ready() -> bool {
    let token = [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    let signed_in = || read_hosts_yml().is_some_and(|y| !signed_in_hosts(&y).is_empty());
    crate::git::gh_available() && (token || signed_in())
}

/// `gh`'s sign-ins. Tests never read the real ones: what they check would
/// change with who runs them.
fn read_hosts_yml() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    std::fs::read_to_string(gh_config_dir()?.join("hosts.yml")).ok()
}

/// The folder `gh` keeps its settings in, found in the same order `gh` looks.
fn gh_config_dir() -> Option<std::path::PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(d) = var("GH_CONFIG_DIR") {
        return Some(d.into());
    }
    if let Some(d) = var("XDG_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(d).join("gh"));
    }
    #[cfg(windows)]
    if let Some(d) = var("AppData") {
        return Some(std::path::PathBuf::from(d).join("GitHub CLI"));
    }
    std::env::home_dir().map(|h| h.join(".config").join("gh"))
}

/// `gh_host` from `GH_HOST` and the text of `gh`'s `hosts.yml`.
fn default_host(env: Option<String>, hosts_yml: Option<&str>) -> String {
    if let Some(h) = env.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) {
        return h;
    }
    match hosts_yml.map(signed_in_hosts).as_deref() {
        Some([only]) => only.clone(),
        _ => "github.com".into(),
    }
}

/// The top-level keys of `hosts.yml`: one per host `gh` is signed in to.
/// Read by hand, as `gh` writes it: `host:` at the start of a line, maybe
/// with a comment after it.
fn signed_in_hosts(yml: &str) -> Vec<String> {
    yml.lines()
        .filter(|l| !l.starts_with([' ', '\t', '#', '-']))
        .map(without_comment)
        .filter_map(|l| l.trim_end().strip_suffix(':'))
        .map(|k| k.trim().trim_matches(['"', '\'']).to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

/// A YAML line without its comment: `#` after a space or a tab.
fn without_comment(l: &str) -> &str {
    l.char_indices()
        .find(|&(i, c)| c == '#' && l[..i].ends_with([' ', '\t']))
        .map_or(l, |(i, _)| &l[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gh_host_is_the_one_gh_reads() {
        let both = "github.com:\n    user: me\n    git_protocol: https\nghe.corp:\n    user: me\n";
        let one = "ghe.corp:\n    users:\n        me:\n    user: me\n";
        assert_eq!(default_host(Some(" ghe.io ".into()), Some(one)), "ghe.io");
        assert_eq!(default_host(None, Some(one)), "ghe.corp");
        assert_eq!(default_host(Some(String::new()), Some(one)), "ghe.corp");
        assert_eq!(default_host(None, Some(both)), "github.com");
        assert_eq!(default_host(None, Some("")), "github.com");
        assert_eq!(default_host(None, None), "github.com");
        let noted = "# gh's sign-ins\nghe.corp: # work\n    user: me # me\n";
        assert_eq!(default_host(None, Some(noted)), "ghe.corp");
        assert_eq!(
            signed_in_hosts("github.com:\t# home\nghe.corp:\n"),
            ["github.com", "ghe.corp"]
        );
    }
}
