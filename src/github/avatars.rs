//! Profile pictures of the people the GitHub tab shows, kept on disk in
//! `github/avatars/` next to `github.json`. A picture is read again after a
//! week. The app has no remote images, so they are downloaded here, off the UI
//! thread, and drawn from the file.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::queue::Details;

/// The size asked of GitHub, in pixels: twice the largest avatar drawn.
pub const SIZE: u32 = 96;
/// A picture older than this is downloaded again.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// The folder the pictures live in.
pub fn dir() -> PathBuf {
    super::file().with_file_name("github").join("avatars")
}

/// The file name of a login's picture: lower case, and only characters every
/// file system takes (`dependabot[bot]` → `dependabot_bot_.png`). None for an
/// empty login (a deleted account).
pub fn file_name(login: &str) -> Option<String> {
    let login = login.trim();
    if login.is_empty() {
        return None;
    }
    let safe: String = login
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    Some(format!("{safe}.png"))
}

pub fn path_in(dir: &Path, login: &str) -> Option<PathBuf> {
    file_name(login).map(|f| dir.join(f))
}

/// Whether a picture written at `modified` should be read again at `now`.
/// A clock that went back counts as fresh.
pub fn stale(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > MAX_AGE)
}

/// The logins to ask GitHub's users API for, in order: as given, then as the
/// app's bot account. Comments by `github-actions` name the bot without its
/// `[bot]`, and only the bot account has the picture.
pub fn candidates(login: &str) -> Vec<String> {
    let login = login.trim();
    if login.is_empty() {
        return Vec::new();
    }
    if login.ends_with("[bot]") {
        return vec![login.to_string()];
    }
    vec![login.to_string(), format!("{login}[bot]")]
}

/// An avatar address at `size` pixels.
pub fn sized(url: &str, size: u32) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}s={size}")
}

/// Everyone a pull request or issue shows: its author, reviewers, commenters,
/// commit authors and assignees, each once, in that order.
pub fn people(d: &Details) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |login: &str| {
        if !login.is_empty() && !out.iter().any(|p| p.eq_ignore_ascii_case(login)) {
            out.push(login.to_string());
        }
    };
    match d {
        Details::Pull(p) => {
            add(&p.author.login);
            p.reviews.iter().for_each(|r| add(&r.author.login));
            p.review_requests.iter().for_each(|r| add(&r.login));
            p.comments.iter().for_each(|c| add(&c.author.login));
            p.inline.iter().for_each(|c| add(&c.user.login));
            p.commits
                .iter()
                .flat_map(|c| c.authors.iter())
                .for_each(|a| add(&a.login));
            p.assignees.iter().for_each(|a| add(&a.login));
        }
        Details::Issue(i) => {
            add(&i.author.login);
            i.comments.iter().for_each(|c| add(&c.author.login));
            i.assignees.iter().for_each(|a| add(&a.login));
        }
    }
    out
}

/// A picture already on disk, and whether it is due for a new download.
pub fn cached(dir: &Path, login: &str) -> Option<(PathBuf, bool)> {
    let path = path_in(dir, login)?;
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    let due = stale(modified, SystemTime::now());
    Some((path, due))
}

/// The address of a login's picture, from GitHub's users API through `gh`.
/// Falls back on the plain address, which works for people but not for bots.
fn address(login: &str) -> String {
    for name in candidates(login) {
        let out = crate::git::gh(&std::env::temp_dir())
            .args(["api", &format!("users/{name}"), "--jq", ".avatar_url"])
            .output();
        if let Ok(out) = out
            && out.status.success()
        {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if url.starts_with("https://") {
                return url;
            }
        }
    }
    format!("https://avatars.githubusercontent.com/{login}")
}

/// Downloads a login's picture into `dir`. Blocks: call it off the UI thread.
pub fn download(dir: &Path, login: &str) -> Result<PathBuf, String> {
    let path = path_in(dir, login).ok_or("no login")?;
    let url = sized(&address(login), SIZE);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(concat!("arakun/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut answer = agent.get(&url).call().map_err(|e| e.to_string())?;
    let bytes = answer
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    // An error page is not a picture: keep the initials instead.
    if image::guess_format(&bytes).is_err() && !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("not an image".into());
    }
    crate::durable::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logins_become_safe_file_names() {
        assert_eq!(
            file_name("RikVisser-dev").as_deref(),
            Some("rikvisser-dev.png")
        );
        assert_eq!(
            file_name("dependabot[bot]").as_deref(),
            Some("dependabot_bot_.png")
        );
        assert_eq!(file_name("../etc").as_deref(), Some("___etc.png"));
        assert_eq!(file_name("  "), None);
        assert_eq!(
            path_in(Path::new("/c"), "ana"),
            Some(PathBuf::from("/c/ana.png"))
        );
    }

    #[test]
    fn pictures_go_stale_after_a_week() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        assert!(!stale(now - Duration::from_secs(3600), now));
        assert!(!stale(now - MAX_AGE, now));
        assert!(stale(now - MAX_AGE - Duration::from_secs(1), now));
        // A file from the future (a clock that moved back) is fresh.
        assert!(!stale(now + Duration::from_secs(60), now));
    }

    #[test]
    fn bots_are_also_asked_for_as_the_bot_account() {
        assert_eq!(
            candidates("github-actions"),
            ["github-actions", "github-actions[bot]"]
        );
        assert_eq!(candidates("renovate[bot]"), ["renovate[bot]"]);
        assert!(candidates("").is_empty());
        assert_eq!(
            sized("https://avatars.githubusercontent.com/in/15368?v=4", 96),
            "https://avatars.githubusercontent.com/in/15368?v=4&s=96"
        );
        assert_eq!(sized("https://a/b", 48), "https://a/b?s=48");
    }

    #[test]
    fn everyone_on_a_pull_request_once() {
        let p: crate::github::pr::Pull = serde_json::from_str(
            r#"{"number":1,"author":{"login":"rik"},
            "reviews":[{"author":{"login":"Ana"},"state":"APPROVED"},{"author":{"login":"ana"},"state":"COMMENTED"}],
            "reviewRequests":[{"login":"bo"},{"slug":"core"}],
            "comments":[{"author":{"login":"github-actions"}}],
            "commits":[{"oid":"a","authors":[{"login":"rik"},{"login":"","name":"Someone"}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            people(&Details::Pull(Box::new(p))),
            ["rik", "Ana", "bo", "github-actions"]
        );
    }
}
