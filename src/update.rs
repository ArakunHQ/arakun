//! Updates: a release build looks for a newer release on GitHub, downloads it,
//! checks it against the release's `SHA256SUMS`, and puts it next to the
//! installed app. The swap happens when Arakun quits (`apply_staged`), so the
//! running app never has its binary replaced under it, and the agents it starts
//! meanwhile run the same version as the window.
//!
//! Only builds made by the release workflow update themselves: it sets
//! `ARAKUN_RELEASE_BUILD=1`. A build from source has features a release does
//! not (the browser), and would lose them. The setting is in Settings ›
//! General; `ARAKUN_UPDATES=0` turns the checks off for one run, and
//! `arakun update` checks and installs from a terminal.
//!
//! The paths it installs to are the ones `tools/install-release.sh` and
//! `tools/install-release.ps1` use, but any place the app runs from and can
//! write to works: the update goes where the running binary is.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

const REPO: &str = "arakunhq/arakun";
/// How often the app looks for a new release.
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// A release download bigger than this is refused.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// Whether the release workflow built this binary.
pub fn release_build() -> bool {
    option_env!("ARAKUN_RELEASE_BUILD") == Some("1")
}

/// What the Settings row shows. Lives for this run only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    /// Downloaded and checked, waiting for Arakun to quit.
    Ready(String),
    Failed(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Status::Idle => "Not checked since Arakun started.".into(),
            Status::Checking => "Looking for a new release…".into(),
            Status::UpToDate => "This is the latest release.".into(),
            Status::Downloading(tag) => format!("Downloading Arakun {tag}…"),
            Status::Ready(tag) => {
                format!("Arakun {tag} is downloaded. It is installed when Arakun quits.")
            }
            Status::Failed(e) => format!("Could not update: {e}."),
        }
    }
}

fn status_cell() -> &'static Mutex<Status> {
    static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
    STATUS.get_or_init(|| {
        Mutex::new(match load().staged {
            Some(tag) => Status::Ready(tag),
            None => Status::Idle,
        })
    })
}

pub fn status() -> Status {
    status_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn set_status(s: Status) {
    *status_cell().lock().unwrap_or_else(|e| e.into_inner()) = s;
}

/// Whether the app checks by itself: on unless the user turned it off in
/// Settings or `ARAKUN_UPDATES=0` says so for this run.
pub fn automatic() -> bool {
    release_build()
        && !matches!(
            std::env::var("ARAKUN_UPDATES").as_deref(),
            Ok("0" | "off" | "false" | "no")
        )
        && !off_marker().exists()
        && crate::dev::size().is_none()
}

/// Turns the automatic checks on or off and remembers it, as a marker file
/// like redacted mode.
pub fn set_automatic(on: bool) -> std::io::Result<()> {
    let path = off_marker();
    if on {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, "do not update arakun by itself\n")
    }
}

fn off_marker() -> PathBuf {
    crate::platform::app_dir().join("no-auto-update")
}

/// `vX.Y.Z` as numbers. Pre-releases (`v1.0.0-rc.1`) are left out: GitHub's
/// latest release never is one, and a tag pinned by hand is the user's call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);

impl Version {
    fn parse(tag: &str) -> Option<Version> {
        let v = tag.strip_prefix('v').unwrap_or(tag);
        let mut parts = v.split('.');
        let mut next = || parts.next()?.parse().ok();
        let version = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0))
    }
}

/// What survives a restart: the update waiting to be installed, and the
/// newest one the user was told about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    staged: Option<String>,
    #[serde(default)]
    notified: Option<String>,
}

fn saved_path() -> PathBuf {
    crate::platform::app_dir().join("update.json")
}

fn load() -> Saved {
    std::fs::read(saved_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(s: &Saved) -> std::io::Result<()> {
    let path = saved_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(s).unwrap_or_default())
}

/// The release this platform's build is called by, and its file type.
fn platform() -> Result<(&'static str, &'static str), String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("linux-x86_64", "tar.gz"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(("macos-arm64", "dmg"))
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Ok(("windows-x86_64", "zip"))
    } else {
        Err("there is no release build for this platform".into())
    }
}

/// What gets replaced: the binary on Linux and Windows, the whole
/// `Arakun.app` on macOS. `staged` is where the new one waits, next to it so
/// the swap is a rename on one disk.
#[derive(Clone, Debug, PartialEq)]
struct Install {
    target: PathBuf,
    staged: PathBuf,
    /// Where the old one goes for the swap (Windows and macOS).
    old: PathBuf,
}

impl Install {
    fn here() -> Result<Install, String> {
        let exe =
            std::env::current_exe().map_err(|e| format!("can't find Arakun's binary: {e}"))?;
        // After a rename-over, Linux names the running binary "… (deleted)".
        let exe = PathBuf::from(
            exe.to_string_lossy()
                .strip_suffix(" (deleted)")
                .map(str::to_string)
                .unwrap_or_else(|| exe.to_string_lossy().into_owned()),
        );
        // ~/.local/bin/arakun links into Arakun.app on macOS.
        let exe = crate::platform::canonicalize(&exe).unwrap_or(exe);
        Install::for_exe(&exe)
    }

    fn for_exe(exe: &Path) -> Result<Install, String> {
        let dir = exe
            .parent()
            .ok_or_else(|| format!("{} has no folder", exe.display()))?;
        if cfg!(target_os = "macos") {
            let app = dir
                .ancestors()
                .nth(2)
                .filter(|_| dir.ends_with("Contents/MacOS"))
                .filter(|a| a.extension().is_some_and(|e| e == "app"))
                .ok_or("Arakun is not running from Arakun.app")?;
            let parent = app.parent().ok_or("Arakun.app has no folder")?;
            Ok(Install {
                target: app.to_path_buf(),
                staged: parent.join(".Arakun.app.update"),
                old: parent.join(".Arakun.app.old"),
            })
        } else {
            let name = exe.file_name().ok_or("the binary has no name")?;
            let name = name.to_string_lossy();
            Ok(Install {
                target: exe.to_path_buf(),
                staged: dir.join(format!(".{name}.update")),
                old: dir.join(format!(".{name}.old")),
            })
        }
    }

    /// The binary inside what is staged, which `--version` is asked of.
    fn staged_binary(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.staged.join("Contents/MacOS/arakun")
        } else {
            self.staged.clone()
        }
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .user_agent(concat!("arakun/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

/// The tag GitHub's latest release redirects to. No API call, so no rate limit.
pub fn latest_tag() -> Result<String, String> {
    use ureq::ResponseExt as _;
    let url = format!("https://github.com/{REPO}/releases/latest");
    let res = agent()
        .head(&url)
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    tag_from_url(&res.get_uri().to_string())
        .ok_or_else(|| format!("no release found at https://github.com/{REPO}/releases"))
}

fn tag_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.split_once("/releases/tag/")?;
    let tag = tag.trim_end_matches('/');
    (!tag.is_empty() && !tag.contains('/')).then(|| tag.to_string())
}

/// The checksum `sums` (a `SHA256SUMS` file) lists for `file`.
fn listed(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file).then(|| hash.to_ascii_lowercase())
    })
}

/// What a check ended in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    /// Downloaded and waiting for Arakun to quit; `true` the first time the
    /// user hears of it.
    Staged(String, bool),
}

/// Looks for a newer release and stages it. Blocking: call it off the UI thread.
pub fn check() -> Result<Outcome, String> {
    let result = check_inner();
    set_status(match &result {
        Ok(Outcome::UpToDate) => Status::UpToDate,
        Ok(Outcome::Staged(tag, _)) => Status::Ready(tag.clone()),
        Err(e) => Status::Failed(e.clone()),
    });
    result
}

fn check_inner() -> Result<Outcome, String> {
    if !release_build() {
        return Err("this build is not a release, so it does not update itself".into());
    }
    set_status(Status::Checking);
    let tag = latest_tag()?;
    let Some(latest) = Version::parse(&tag) else {
        return Ok(Outcome::UpToDate);
    };
    if latest <= Version::current() {
        return Ok(Outcome::UpToDate);
    }
    let mut saved = load();
    let install = Install::here()?;
    let ready = saved.staged.as_deref() == Some(tag.as_str()) && install.staged.exists();
    if !ready {
        set_status(Status::Downloading(tag.clone()));
        stage(&tag, &install)?;
        saved.staged = Some(tag.clone());
    }
    let first = saved.notified.as_deref() != Some(tag.as_str());
    saved.notified = Some(tag.clone());
    save(&saved).map_err(|e| format!("could not save the update's state: {e}"))?;
    Ok(Outcome::Staged(tag, first))
}

/// Downloads `tag`, checks it, and puts it at `install.staged`.
fn stage(tag: &str, install: &Install) -> Result<(), String> {
    let (platform, ext) = platform()?;
    let name = format!("arakun-{tag}-{platform}");
    let file = format!("{name}.{ext}");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");

    let sums = agent()
        .get(format!("{base}/SHA256SUMS"))
        .call()
        .and_then(|r| r.into_body().read_to_string())
        .map_err(|e| format!("could not download {tag}'s SHA256SUMS: {e}"))?;
    let want = listed(&sums, &file).ok_or_else(|| format!("{tag} has no {platform} build"))?;

    let work = crate::platform::app_dir().join("update-download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let result = (|| {
        let archive = work.join(&file);
        let have = download(&format!("{base}/{file}"), &archive)?;
        if have != want {
            return Err(format!(
                "checksum mismatch for {file} (expected {want}, got {have})"
            ));
        }
        remove_any(&install.staged).map_err(|e| format!("{}: {e}", install.staged.display()))?;
        unpack(&archive, &work, &name, &install.staged)?;
        verify(&install.staged_binary(), tag)
    })();
    let _ = std::fs::remove_dir_all(&work);
    if result.is_err() {
        let _ = remove_any(&install.staged);
    }
    result
}

/// Saves `url` to `to` and returns its SHA-256 in hex.
fn download(url: &str, to: &Path) -> Result<String, String> {
    let res = agent()
        .get(url)
        .call()
        .map_err(|e| format!("could not download {url}: {e}"))?;
    let mut body = res
        .into_body()
        .into_with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    let mut out = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut hash = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = body
            .read(&mut buf)
            .map_err(|e| format!("download of {url} broke off: {e}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    out.sync_all()
        .map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Takes the app out of `archive` and puts it at `to`.
fn unpack(archive: &Path, work: &Path, name: &str, to: &Path) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let mnt = work.join("mnt");
        run(crate::platform::command("hdiutil")
            .args([
                "attach",
                "-quiet",
                "-readonly",
                "-nobrowse",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mnt)
            .arg(archive))?;
        let copied = run(crate::platform::command("ditto")
            .arg(mnt.join("Arakun.app"))
            .arg(to));
        // Read-only, so forcing it loses nothing.
        let detach = |force: bool| {
            let mut cmd = crate::platform::command("hdiutil");
            cmd.args(["detach", "-quiet"]);
            if force {
                cmd.arg("-force");
            }
            run(cmd.arg(&mnt))
        };
        let _ = detach(false).or_else(|_| detach(true));
        return copied;
    }
    // GNU tar on Linux; Windows 10 and later ship bsdtar, which opens zips too.
    let x = work.join("x");
    std::fs::create_dir_all(&x).map_err(|e| format!("{}: {e}", x.display()))?;
    let flags = if cfg!(windows) { "-xf" } else { "-xzf" };
    run(crate::platform::command("tar")
        .arg("-C")
        .arg(&x)
        .arg(flags)
        .arg(archive))?;
    let bin = if cfg!(windows) {
        "arakun.exe"
    } else {
        "arakun"
    };
    let from = x.join(name).join(bin);
    if !from.is_file() {
        return Err(format!("the download has no {bin}"));
    }
    std::fs::copy(&from, to).map_err(|e| format!("{}: {e}", to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// The staged binary starts on this system and is the version it says.
fn verify(bin: &Path, tag: &str) -> Result<(), String> {
    let out = crate::platform::command(bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("the new version does not start: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    let want = format!("arakun {}", tag.trim_start_matches('v'));
    if out.status.success() && said.trim() == want {
        Ok(())
    } else {
        Err(format!(
            "the new version does not start here (it said {:?})",
            said.trim()
        ))
    }
}

fn run(cmd: &mut std::process::Command) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    let result = match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Puts a staged update in place of the running app. Called as Arakun quits,
/// before a restart starts it again. `Some(tag)` when it installed one.
pub fn apply_staged() -> Result<Option<String>, String> {
    if !release_build() {
        return Ok(None);
    }
    let mut saved = load();
    let Some(tag) = saved.staged.clone() else {
        return Ok(None);
    };
    let install = Install::here()?;
    let newer = Version::parse(&tag).is_some_and(|v| v > Version::current());
    if newer && install.staged.exists() {
        swap(&install).map_err(|e| format!("could not install Arakun {tag}: {e}"))?;
    } else {
        let _ = remove_any(&install.staged);
    }
    saved.staged = None;
    save(&saved).map_err(|e| format!("could not save the update's state: {e}"))?;
    Ok(newer.then_some(tag))
}

fn swap(install: &Install) -> std::io::Result<()> {
    // Linux and macOS replace a file with a rename even while it runs. Windows
    // locks a running .exe against that, but lets it be renamed away.
    if cfg!(target_os = "linux") {
        return std::fs::rename(&install.staged, &install.target);
    }
    remove_any(&install.old)?;
    std::fs::rename(&install.target, &install.old)?;
    if let Err(e) = std::fs::rename(&install.staged, &install.target) {
        let _ = std::fs::rename(&install.old, &install.target);
        return Err(e);
    }
    // On Windows the old .exe is still running; the next start removes it.
    let _ = remove_any(&install.old);
    Ok(())
}

/// Removes what an earlier update left behind (Windows' old .exe).
pub fn clean_up() {
    if !release_build() {
        return;
    }
    if let Ok(install) = Install::here() {
        let _ = remove_any(&install.old);
    }
}

const USAGE: &str = "\
arakun update           download the latest release and install it
arakun update --check   only say whether there is a newer release";

/// `arakun update`.
pub fn cli(args: &[String]) -> i32 {
    let check_only = match args {
        [] => false,
        [a] if a == "--check" => true,
        [a] if a == "-h" || a == "--help" => {
            println!("{USAGE}");
            return 0;
        }
        _ => {
            eprintln!("usage:\n{USAGE}");
            return 2;
        }
    };
    if !release_build() {
        eprintln!(
            "arakun: this build is not a release, so it does not update itself; \
             pull and build it again, or install a release (see README.md)"
        );
        return 1;
    }
    let current = env!("CARGO_PKG_VERSION");
    if check_only {
        return match latest_tag() {
            Ok(tag) if Version::parse(&tag).is_some_and(|v| v > Version::current()) => {
                println!(
                    "Arakun {tag} is out; this is {current}. Run arakun update to install it."
                );
                0
            }
            Ok(_) => {
                println!("Arakun {current} is the latest release.");
                0
            }
            Err(e) => {
                eprintln!("arakun: {e}");
                1
            }
        };
    }
    println!("arakun: looking for a new release");
    match check() {
        Ok(Outcome::UpToDate) => {
            println!("arakun: {current} is the latest release");
            0
        }
        Ok(Outcome::Staged(tag, _)) if crate::control::app_running() => {
            println!(
                "arakun: Arakun {tag} is downloaded; it is installed when Arakun quits \
                 (arakun restart does that now)"
            );
            0
        }
        Ok(Outcome::Staged(..)) => match apply_staged() {
            Ok(Some(tag)) => {
                println!("arakun: installed Arakun {tag}");
                0
            }
            Ok(None) => 0,
            Err(e) => {
                eprintln!("arakun: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("arakun: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(Version::parse("v0.2.1"), Some(Version(0, 2, 1)));
        assert_eq!(Version::parse("1.10.0"), Some(Version(1, 10, 0)));
        assert!(Version::parse("v0.10.0") > Version::parse("v0.9.9"));
        assert_eq!(Version::parse("v1.0.0-rc.1"), None);
        assert_eq!(Version::parse("v1.0"), None);
        assert_eq!(Version::parse("v1.0.0.1"), None);
        assert_eq!(Version::parse("latest"), None);
    }

    #[test]
    fn the_tag_comes_from_the_redirect() {
        assert_eq!(
            tag_from_url("https://github.com/arakunhq/arakun/releases/tag/v0.3.0").as_deref(),
            Some("v0.3.0")
        );
        // No release yet: GitHub stays on /releases.
        assert_eq!(
            tag_from_url("https://github.com/arakunhq/arakun/releases"),
            None
        );
        assert_eq!(
            tag_from_url("https://github.com/arakunhq/arakun/releases/tag/"),
            None
        );
    }

    #[test]
    fn checksums_are_found_by_file_name() {
        let sums = "\
ABC123  arakun-v0.3.0-linux-x86_64.tar.gz
def456 *arakun-v0.3.0-windows-x86_64.zip
789aaa  arakun-v0.3.0-macos-arm64.dmg
";
        assert_eq!(
            listed(sums, "arakun-v0.3.0-linux-x86_64.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            listed(sums, "arakun-v0.3.0-windows-x86_64.zip").as_deref(),
            Some("def456")
        );
        assert_eq!(listed(sums, "arakun-v0.3.0-linux-x86_64"), None);
    }

    #[test]
    fn the_update_waits_next_to_what_it_replaces() {
        if cfg!(target_os = "macos") {
            let i = Install::for_exe(Path::new(
                "/Users/a/Applications/Arakun.app/Contents/MacOS/arakun",
            ))
            .unwrap();
            assert_eq!(i.target, Path::new("/Users/a/Applications/Arakun.app"));
            assert_eq!(
                i.staged,
                Path::new("/Users/a/Applications/.Arakun.app.update")
            );
            assert!(Install::for_exe(Path::new("/Users/a/bin/arakun")).is_err());
        } else {
            let dir = std::env::temp_dir();
            let i = Install::for_exe(&dir.join("arakun")).unwrap();
            assert_eq!(i.target, dir.join("arakun"));
            assert_eq!(i.staged, dir.join(".arakun.update"));
            assert_eq!(i.staged_binary(), i.staged);
        }
    }

    #[test]
    fn a_swap_puts_the_new_binary_in_place() {
        let dir = std::env::temp_dir().join(format!("arakun-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let install = Install {
            target: dir.join("arakun"),
            staged: dir.join(".arakun.update"),
            old: dir.join(".arakun.old"),
        };
        std::fs::write(&install.target, "old").unwrap();
        std::fs::write(&install.staged, "new").unwrap();
        swap(&install).unwrap();
        assert_eq!(std::fs::read_to_string(&install.target).unwrap(), "new");
        assert!(!install.staged.exists());
        assert!(!install.old.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn statuses_read_as_sentences() {
        for s in [
            Status::Idle,
            Status::Checking,
            Status::UpToDate,
            Status::Downloading("v1.0.0".into()),
            Status::Ready("v1.0.0".into()),
            Status::Failed("no network".into()),
        ] {
            let t = s.text();
            assert!(t.ends_with('.') || t.ends_with('…'), "{t}");
        }
    }
}
