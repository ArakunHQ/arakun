//! Updates: a release build looks for a newer release on GitHub, downloads it,
//! checks it against the release's `SHA256SUMS`, and puts it next to the
//! installed app. `SHA256SUMS` must carry a signature by the release key
//! (`assets/release-key.pub`, or the offline backup next to it), so a release
//! changed by someone without that key is refused. The swap happens when Arakun quits (`apply_staged`), so the
//! running app never has its binary replaced under it, and the agents it starts
//! meanwhile run the same version as the window.
//!
//! Only builds made by the release workflow update themselves: it sets
//! `ARAKUN_RELEASE_BUILD=1`. A build from source is installed its own way
//! (tools/install.sh), with the browser runtime it was built against, and an
//! update would put a release's binary in it. The setting is in Settings ›
//! General; `ARAKUN_UPDATES=0` turns the checks off for one run, and
//! `arakun update` checks and installs from a terminal.
//!
//! Two channels: stable takes GitHub's latest release, nightly takes whichever
//! is newest of the releases and the nightly builds `release.yml` publishes
//! from main as pre-releases (`0.5.1-nightly.202611180300`). A nightly is
//! signed with its own key (`assets/nightly-key.pub`), which is only accepted
//! for a nightly version, so it can never sign a stable release. Leaving the
//! nightly channel keeps the nightly until a release newer than it comes out.
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
/// The minisign keys a release's `SHA256SUMS` may be signed with: the one
/// `release.yml` uses, and a backup kept offline, so losing the first one does
/// not strand every installed updater. The install scripts carry the same
/// keys; a test keeps them in step.
const RELEASE_KEYS: [&str; 2] = [
    include_str!("../assets/release-key.pub"),
    include_str!("../assets/release-key-backup.pub"),
];
/// The key of the nightly builds, which `release.yml` keeps in the
/// `nightly-signing` environment. Accepted for nightly versions only.
const NIGHTLY_KEY: &str = include_str!("../assets/nightly-key.pub");

/// The keys that may sign `SHA256SUMS` of the build `version`.
fn keys_for(version: Version) -> Vec<&'static str> {
    let mut keys = RELEASE_KEYS.to_vec();
    if version.is_nightly() {
        keys.push(NIGHTLY_KEY);
    }
    keys
}

/// Which builds the updater installs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    fn parse(s: &str) -> Option<Channel> {
        match s.trim() {
            "stable" => Some(Channel::Stable),
            "nightly" => Some(Channel::Nightly),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        }
    }
}

/// The channel the user picked, or else the one this build came from, so a
/// nightly installed by hand keeps getting nightlies.
pub fn channel() -> Channel {
    std::fs::read_to_string(channel_file())
        .ok()
        .and_then(|s| Channel::parse(&s))
        .unwrap_or(if Version::current().is_nightly() {
            Channel::Nightly
        } else {
            Channel::Stable
        })
}

pub fn set_channel(channel: Channel) -> std::io::Result<()> {
    let path = channel_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Waits out an install that is checking the channel (see apply_locked).
    let _held = channel_lock()?;
    crate::durable::write(&path, format!("{}\n", channel.name()).as_bytes())?;
    // What the last check found was for the other channel.
    set_status(Status::Idle);
    Ok(())
}

/// Held while the channel is written, and by an install from its channel check
/// to its swap, so a switch to stable can't land between the two and let a
/// nightly in. Not the update lock: that one is held through a download, and
/// a switch in Settings must not wait for it. Blocking, but only for a hash
/// and a rename.
fn channel_lock() -> std::io::Result<std::fs::File> {
    let path = crate::platform::app_dir().join("update-channel.lock");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    file.lock()?;
    Ok(file)
}

fn channel_file() -> PathBuf {
    crate::platform::app_dir().join("update-channel")
}

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
            Status::UpToDate => match channel() {
                Channel::Stable => "This is the latest release.".into(),
                Channel::Nightly => "This is the latest nightly build.".into(),
            },
            Status::Downloading(tag) => format!("Downloading Arakun {tag}…"),
            // Downloaded for the nightly channel, which the user has left
            // since: apply_locked drops it.
            Status::Ready(tag) if !will_install(tag) => {
                format!("Arakun {tag} was downloaded for nightly builds, so it is not installed.")
            }
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

/// `vX.Y.Z` as numbers, or a nightly `X.Y.Z-nightly.N`, which comes before
/// `X.Y.Z` and after every older nightly of it. Other pre-releases
/// (`v1.0.0-rc.1`) are left out: GitHub's latest release never is one, and a
/// tag pinned by hand is the user's call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64, Pre);

/// Declared in this order so a nightly sorts before its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pre {
    Nightly(u64),
    Release,
}

impl Version {
    fn parse(tag: &str) -> Option<Version> {
        let v = tag.strip_prefix('v').unwrap_or(tag);
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => {
                let n = pre.strip_prefix("nightly.")?;
                if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                (core, Pre::Nightly(n.parse().ok()?))
            }
            None => (v, Pre::Release),
        };
        let mut parts = core.split('.');
        let mut next = || parts.next()?.parse().ok();
        let version = Version(next()?, next()?, next()?, pre);
        parts.next().is_none().then_some(version)
    }

    fn current() -> Version {
        Version::parse(env!("ARAKUN_VERSION")).unwrap_or(Version(0, 0, 0, Pre::Release))
    }

    fn is_nightly(self) -> bool {
        matches!(self.3, Pre::Nightly(_))
    }
}

/// What survives a restart: the update waiting to be installed, and the
/// newest one the user was told about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    staged: Option<String>,
    /// The staged binary's SHA-256, checked again before it is installed.
    #[serde(default)]
    staged_sha256: Option<String>,
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
    crate::durable::write(
        &saved_path(),
        &serde_json::to_vec_pretty(s).unwrap_or_default(),
    )
}

/// The release this platform's build is called by, and its file type.
fn platform() -> Result<(&'static str, &'static str), String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("linux-x86_64", "tar.gz"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok(("linux-arm64", "tar.gz"))
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
/// the swap is a rename on one disk. It is unpacked as `incoming` and renamed
/// to `staged` once it is complete and starts, so nothing half-written ever
/// sits at `staged`.
#[derive(Clone, Debug, PartialEq)]
struct Install {
    target: PathBuf,
    staged: PathBuf,
    incoming: PathBuf,
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
        let install = Install::for_exe(&exe)?;
        // A package manager installs as root: leave its files to it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let owner = std::fs::metadata(&install.target)
                .map_err(|e| format!("{}: {e}", install.target.display()))?
                .uid();
            // SAFETY: geteuid has no preconditions and cannot fail.
            if owner != unsafe { libc::geteuid() } {
                return Err(format!(
                    "{} belongs to another user or a package manager; update it the way it was installed",
                    install.target.display()
                ));
            }
        }
        Ok(install)
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
                incoming: parent.join(".Arakun.app.incoming"),
                old: parent.join(".Arakun.app.old"),
            })
        } else {
            let name = exe.file_name().ok_or("the binary has no name")?;
            let name = name.to_string_lossy();
            Ok(Install {
                target: exe.to_path_buf(),
                staged: dir.join(format!(".{name}.update")),
                incoming: dir.join(format!(".{name}.incoming")),
                old: dir.join(format!(".{name}.old")),
            })
        }
    }

    /// The binary inside `root` (`staged` or `incoming`).
    fn binary_in(&self, root: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            root.join("Contents/MacOS/arakun")
        } else {
            root.to_path_buf()
        }
    }

    /// Whether `staged` holds the binary `saved` recorded, whole and unchanged.
    fn staged_intact(&self, saved: &Saved) -> bool {
        saved.staged_sha256.as_deref().is_some_and(|want| {
            hash_file(&self.binary_in(&self.staged)).is_ok_and(|have| have == want)
        })
    }
}

/// Held while one updater checks, downloads or installs, so `arakun update`,
/// Check now and the app's own checks never remove each other's files.
struct Lock {
    _file: std::fs::File,
}

/// `None` while another updater holds it.
fn lock() -> Result<Option<Lock>, String> {
    let path = crate::platform::app_dir().join("update.lock");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lock { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex(&hash.finalize()));
        }
        hash.update(&buf[..n]);
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_resolve(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_send_request(Some(Duration::from_secs(30)))
            // A server that takes the request and never answers would hold the
            // update lock, and Check now, forever.
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
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

/// The newest build on `channel`, as a tag and its version. `None` when the
/// newest one GitHub has is not newer than this build or is not a version.
fn newest(channel: Channel) -> Result<Option<(String, Version)>, String> {
    let found = match channel {
        Channel::Stable => {
            let tag = latest_tag()?;
            Version::parse(&tag)
                .filter(|v| !v.is_nightly())
                .map(|v| (tag, v))
        }
        Channel::Nightly => newest_listed(&list_releases()?),
    };
    Ok(found.filter(|(_, v)| *v > Version::current()))
}

/// The newest releases and pre-releases, as GitHub's API lists them. The
/// nightly channel only: the API allows 60 calls an hour from one address,
/// which checks every few hours stay far below.
fn list_releases() -> Result<String, String> {
    agent()
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases?per_page=30"
        ))
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|r| r.into_body().read_to_string())
        .map_err(|e| format!("could not list the releases on GitHub: {e}"))
}

/// The highest version among the published releases in `json`, a list from
/// GitHub's releases API.
fn newest_listed(json: &str) -> Option<(String, Version)> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        #[serde(default)]
        draft: bool,
    }
    let releases: Vec<Release> = serde_json::from_str(json).ok()?;
    releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| Version::parse(&r.tag_name).map(|v| (r.tag_name, v)))
        .max_by_key(|(_, v)| *v)
}

fn tag_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.split_once("/releases/tag/")?;
    let tag = tag.trim_end_matches('/');
    (!tag.is_empty() && !tag.contains('/')).then(|| tag.to_string())
}

/// Whether `sig` (a `SHA256SUMS.minisig`) is a signature of `sums` by one of
/// `keys`. Only the prehashed form `minisign` makes by default is accepted.
fn signed(keys: &[&str], sums: &[u8], sig: &str) -> Result<(), String> {
    let sig = minisign_verify::Signature::decode(sig)
        .map_err(|e| format!("SHA256SUMS.minisig can't be read: {e}"))?;
    let mut error = String::from("no release key");
    for key in keys {
        let key = minisign_verify::PublicKey::decode(key)
            .map_err(|e| format!("a release key built into Arakun can't be read: {e}"))?;
        match key.verify(sums, &sig, false) {
            Ok(()) => return Ok(()),
            Err(e) => error = e.to_string(),
        }
    }
    Err(format!(
        "SHA256SUMS is not signed with Arakun's release key ({error})"
    ))
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
    let result = match lock() {
        Ok(Some(_lock)) => check_locked(),
        Ok(None) => Err("another Arakun is updating already".into()),
        Err(e) => Err(e),
    };
    set_status(match &result {
        Ok(Outcome::UpToDate) => Status::UpToDate,
        Ok(Outcome::Staged(tag, _)) => Status::Ready(tag.clone()),
        Err(e) => Status::Failed(e.clone()),
    });
    result
}

fn check_locked() -> Result<Outcome, String> {
    if !release_build() {
        return Err("this build is not a release, so it does not update itself".into());
    }
    set_status(Status::Checking);
    let Some((tag, version)) = newest(channel())? else {
        return Ok(Outcome::UpToDate);
    };
    let mut saved = load();
    let install = Install::here()?;
    let ready = saved.staged.as_deref() == Some(tag.as_str()) && install.staged_intact(&saved);
    if !ready {
        set_status(Status::Downloading(tag.clone()));
        stage(&tag, version, &install)?;
        // Forget the old update before its file goes, so a quit from here on
        // finds nothing to install rather than something half-replaced.
        saved.staged = None;
        saved.staged_sha256 = None;
        save(&saved).map_err(|e| format!("could not save the update's state: {e}"))?;
        promote(&install).map_err(|e| format!("{}: {e}", install.staged.display()))?;
        let hash = hash_file(&install.binary_in(&install.staged))
            .map_err(|e| format!("{}: {e}", install.staged.display()))?;
        saved.staged = Some(tag.clone());
        saved.staged_sha256 = Some(hash);
    }
    let first = saved.notified.as_deref() != Some(tag.as_str());
    saved.notified = Some(tag.clone());
    save(&saved).map_err(|e| format!("could not save the update's state: {e}"))?;
    Ok(Outcome::Staged(tag, first))
}

/// Downloads `tag`, checks it, and puts it at `install.incoming`.
fn stage(tag: &str, version: Version, install: &Install) -> Result<(), String> {
    let (platform, ext) = platform()?;
    let name = format!("arakun-{tag}-{platform}");
    let file = format!("{name}.{ext}");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");

    let fetch = |what: &str| {
        agent()
            .get(format!("{base}/{what}"))
            .call()
            .and_then(|r| r.into_body().read_to_string())
            .map_err(|e| format!("could not download {tag}'s {what}: {e}"))
    };
    let sums = fetch("SHA256SUMS")?;
    // Before any checksum in it is trusted. Every release after v0.3.0 is
    // signed, and the updater only installs newer ones than itself.
    let sig = fetch("SHA256SUMS.minisig")?;
    signed(&keys_for(version), sums.as_bytes(), &sig).map_err(|e| format!("{tag}: {e}"))?;
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
        remove_any(&install.incoming)
            .map_err(|e| format!("{}: {e}", install.incoming.display()))?;
        let _ = remove_any(&pdb_beside(&install.incoming));
        unpack(&archive, &work, &name, &install.incoming)?;
        verify(&install.binary_in(&install.incoming), tag)
    })();
    let _ = std::fs::remove_dir_all(&work);
    if result.is_err() {
        let _ = remove_any(&install.incoming);
        let _ = remove_any(&pdb_beside(&install.incoming));
    }
    result
}

/// Moves a complete, checked `incoming` to `staged`.
fn promote(install: &Install) -> std::io::Result<()> {
    remove_any(&install.staged)?;
    std::fs::rename(&install.incoming, &install.staged)?;
    // Only crash reports need it: a .pdb that won't move doesn't stop the
    // update, and a stale one beside `staged` doesn't match the new binary.
    let staged_pdb = pdb_beside(&install.staged);
    let _ = remove_any(&staged_pdb);
    let _ = move_pdb(&pdb_beside(&install.incoming), &staged_pdb);
    Ok(())
}

/// Where a staged or incoming Windows binary's .pdb waits: its name plus
/// ".pdb". Crash reports read the function names of arakun.exe from the
/// arakun.pdb beside it, and it must match the binary, so it moves with it.
fn pdb_beside(binary: &Path) -> PathBuf {
    let mut name = binary.as_os_str().to_owned();
    name.push(".pdb");
    PathBuf::from(name)
}

/// Moves a .pdb if there is one: earlier releases shipped none.
fn move_pdb(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_file() {
        return Ok(());
    }
    remove_any(to)?;
    std::fs::rename(from, to)
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
    Ok(hex(&hash.finalize()))
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
    run(tar().arg("-C").arg(&x).arg(flags).arg(archive))?;
    let bin = if cfg!(windows) {
        "arakun.exe"
    } else {
        "arakun"
    };
    let from = x.join(name).join(bin);
    if !from.is_file() {
        return Err(format!("the download has no {bin}"));
    }
    let exe_dir = to.parent().ok_or("the binary has no folder")?;
    update_runtime(&x.join(name), exe_dir, &crate::platform::app_dir())?;
    std::fs::copy(&from, to).map_err(|e| format!("{}: {e}", to.display()))?;
    let pdb = x.join(name).join("arakun.pdb");
    if cfg!(windows) && pdb.is_file() {
        // Only crash reports need it: without it the binary still stages.
        let at = pdb_beside(to);
        if std::fs::copy(&pdb, &at).is_err() {
            let _ = remove_any(&at);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// The browser's runtime (CEF) of a release: a `cef` folder next to the binary
/// on Linux, files next to arakun.exe on Windows. Its `archive.json` names the
/// CEF build, which must be the one the binary was built against.
///
/// Only the binary is swapped when Arakun quits, so the runtime is handled
/// here: installed when there is none yet (a release from before the browser),
/// left alone when it is the same build, and refused when it is another one.
/// Windows keeps the files of a running browser locked, and Arakun applies an
/// update while it still runs, so a new runtime takes the install script.
#[derive(Debug, PartialEq, Eq)]
enum RuntimeStep {
    Nothing,
    Install,
    Mismatch,
}

/// `new` and `installed` are the `archive.json` of the download's runtime and
/// of the installed one, `None` where there is no runtime.
fn runtime_step(new: Option<&str>, installed: Option<&str>) -> RuntimeStep {
    match (new, installed) {
        (None, _) => RuntimeStep::Nothing,
        (Some(_), None) => RuntimeStep::Install,
        (Some(a), Some(b)) if a.trim() == b.trim() => RuntimeStep::Nothing,
        _ => RuntimeStep::Mismatch,
    }
}

/// The library that makes a folder a runtime, and the file that names its build.
const LIBCEF: &str = if cfg!(windows) {
    "libcef.dll"
} else {
    "libcef.so"
};

/// Where this install's runtime is: on Linux a `cef` folder next to the binary,
/// as in a release archive, else the data folder's (where src/browser/loader.rs
/// looks, and tools/install.sh puts it); on Windows the binary's own folder.
fn installed_runtime(exe_dir: &Path, app_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        return exe_dir.to_path_buf();
    }
    let next = exe_dir.join("cef");
    if next.join(LIBCEF).is_file() {
        next
    } else {
        app_dir.join("cef")
    }
}

/// The `archive.json` of the runtime in `dir`, or `None` if `dir` has none. A
/// runtime without one counts as another build.
fn runtime_build(dir: &Path) -> Option<String> {
    dir.join(LIBCEF)
        .is_file()
        .then(|| std::fs::read_to_string(dir.join("archive.json")).unwrap_or_default())
}

/// Brings the runtime of the unpacked release in `root` to this install; see
/// [`RuntimeStep`].
fn update_runtime(root: &Path, exe_dir: &Path, app_dir: &Path) -> Result<(), String> {
    let new_dir = if cfg!(windows) {
        root.to_path_buf()
    } else {
        root.join("cef")
    };
    let target = installed_runtime(exe_dir, app_dir);
    let new = runtime_build(&new_dir);
    let installed = runtime_build(&target);
    match runtime_step(new.as_deref(), installed.as_deref()) {
        RuntimeStep::Nothing => Ok(()),
        RuntimeStep::Install => install_runtime(&new_dir, &target).map_err(|e| {
            format!(
                "could not install the browser runtime in {}: {e}",
                target.display()
            )
        }),
        RuntimeStep::Mismatch => Err(format!(
            "this release comes with a new version of the browser, which an update can't \
             install. Quit Arakun and install it again: {}",
            if cfg!(windows) {
                "irm https://arakun.dev/install.ps1 | iex"
            } else {
                "curl -fsSL https://arakun.dev/install | sh"
            }
        )),
    }
}

/// Puts a runtime where there is none, so nothing in `to` is in use. Linux
/// moves the folder in one rename. Windows copies the files next to arakun.exe,
/// the library last, so a copy cut short is not taken for a runtime.
fn install_runtime(from: &Path, to: &Path) -> std::io::Result<()> {
    if !cfg!(windows) {
        remove_any(to)?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return std::fs::rename(from, to).or_else(|_| copy_tree(from, to));
    }
    // The binary and its .pdb are swapped at quit, and the papers are not the
    // runtime's.
    let skip = [
        "arakun.exe",
        "arakun.pdb",
        "LICENSE",
        "NOTICE",
        "README.md",
        LIBCEF,
    ];
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    std::fs::copy(from.join(LIBCEF), to.join(LIBCEF))?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
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

/// Windows' own bsdtar. Git for Windows' GNU tar may come first on `PATH`: it
/// can't open a zip, and reads `C:\…` as a remote host.
fn tar() -> std::process::Command {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        return crate::platform::command(Path::new(&root).join("System32").join("tar.exe"));
    }
    crate::platform::command("tar")
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
    match lock()? {
        Some(_lock) => apply_locked(),
        // Another updater is busy; what it stages is installed at the next quit.
        None => Ok(None),
    }
}

/// Whether a staged `tag` may replace `current`: it must be newer, and a
/// nightly only while the user follows nightlies, since one staged before a
/// switch to stable must not be installed after it.
fn installable(tag: &str, channel: Channel, current: Version) -> bool {
    Version::parse(tag)
        .is_some_and(|v| v > current && (!v.is_nightly() || channel == Channel::Nightly))
}

/// Whether a staged `tag` would be installed at quit on the current channel.
pub fn will_install(tag: &str) -> bool {
    installable(tag, channel(), Version::current())
}

fn apply_locked() -> Result<Option<String>, String> {
    let mut saved = load();
    let Some(tag) = saved.staged.clone() else {
        return Ok(None);
    };
    let install = Install::here()?;
    // Until the swap is done: the channel read here is the one it installs for.
    let _channel = channel_lock().map_err(|e| format!("could not lock the update channel: {e}"))?;
    let intact = installable(&tag, channel(), Version::current()) && install.staged_intact(&saved);
    if intact {
        swap(&install).map_err(|e| format!("could not install Arakun {tag}: {e}"))?;
    } else {
        let _ = remove_any(&install.staged);
        let _ = remove_any(&pdb_beside(&install.staged));
    }
    saved.staged = None;
    saved.staged_sha256 = None;
    save(&saved).map_err(|e| format!("could not save the update's state: {e}"))?;
    Ok(intact.then_some(tag))
}

fn swap(install: &Install) -> std::io::Result<()> {
    // Linux and macOS replace a file with a rename even while it runs. Windows
    // locks a running .exe against that, but lets it be renamed away.
    if cfg!(target_os = "linux") {
        return std::fs::rename(&install.staged, &install.target);
    }
    // The island host can still run the .exe an earlier update left here (it
    // outlives the app), and Windows won't delete it then. Moved aside, it is
    // removed by a later start, once nothing runs it.
    if remove_any(&install.old).is_err() {
        std::fs::rename(&install.old, aside(&install.old))?;
    }
    std::fs::rename(&install.target, &install.old)?;
    if let Err(e) = std::fs::rename(&install.staged, &install.target) {
        let _ = std::fs::rename(&install.old, &install.target);
        return Err(e);
    }
    // On Windows the old .exe is still running; the next start removes it.
    let _ = remove_any(&install.old);
    // Only crash reports need it: a .pdb that won't move doesn't stop the update.
    let _ = move_pdb(
        &pdb_beside(&install.staged),
        &install.target.with_extension("pdb"),
    );
    Ok(())
}

/// A free name next to `old`, for an old .exe that can't be removed yet.
fn aside(old: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut name = old.as_os_str().to_owned();
    name.push(format!("-{nanos}"));
    PathBuf::from(name)
}

/// Removes the old .exe an update left behind, and those moved `aside` while
/// they still ran. One that still runs stays for the next start.
fn remove_old(install: &Install) {
    let _ = remove_any(&install.old);
    let (Some(dir), Some(name)) = (install.old.parent(), install.old.file_name()) else {
        return;
    };
    let prefix = format!("{}-", name.to_string_lossy());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        // Only the names `aside` makes: anything else is someone else's.
        let file = entry.file_name();
        let moved = file
            .to_string_lossy()
            .strip_prefix(&prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if moved {
            let _ = remove_any(&entry.path());
        }
    }
}

/// Removes what an earlier update left behind: Windows' old .exe, and a
/// download a crash or a quit cut short. Not while another updater runs.
pub fn clean_up() {
    if !release_build() {
        return;
    }
    if let (Ok(Some(_lock)), Ok(install)) = (lock(), Install::here()) {
        remove_old(&install);
        let _ = remove_any(&install.incoming);
        let _ = remove_any(&pdb_beside(&install.incoming));
    }
}

const USAGE: &str = "\
arakun update                     download the latest release and install it
arakun update --check             only say whether there is a newer release
arakun update --channel           say which channel this install follows
arakun update --channel nightly   follow the nightly builds of main (they can break)
arakun update --channel stable    follow the releases again";

const NIGHTLY_WARNING: &str = "\
Nightly builds are made from main every night, untested by hand. They can
crash, lose a session or change how your data is stored, and going back to a
release may not read what a nightly wrote. Use them to try what is next and
report what breaks, not for work you can't redo.";

fn latest_what() -> &'static str {
    match channel() {
        Channel::Stable => "release",
        Channel::Nightly => "build on the nightly channel",
    }
}

/// `arakun update`.
pub fn cli(args: &[String]) -> i32 {
    let check_only = match args {
        [] => false,
        [a] if a == "--check" => true,
        [a] if a == "--channel" => {
            println!("{}", channel().name());
            return 0;
        }
        [a, name] if a == "--channel" => {
            let Some(to) = Channel::parse(name) else {
                eprintln!("arakun: the channels are stable and nightly, not {name:?}");
                return 2;
            };
            if let Err(e) = set_channel(to) {
                eprintln!("arakun: could not save the channel: {e}");
                return 1;
            }
            match to {
                Channel::Nightly => {
                    eprintln!("warning: {NIGHTLY_WARNING}");
                    println!(
                        "arakun: following the nightly builds; arakun update installs the newest"
                    );
                }
                Channel::Stable if Version::current().is_nightly() => println!(
                    "arakun: following the releases; this nightly stays until a newer release is out, \
                     or install the latest release again to go back now"
                ),
                Channel::Stable => println!("arakun: following the releases"),
            }
            return 0;
        }
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
    let current = env!("ARAKUN_VERSION");
    if check_only {
        return match newest(channel()) {
            Ok(Some((tag, _))) => {
                println!(
                    "Arakun {tag} is out; this is {current}. Run arakun update to install it."
                );
                0
            }
            Ok(None) => {
                println!("Arakun {current} is the latest {}.", latest_what());
                0
            }
            Err(e) => {
                eprintln!("arakun: {e}");
                1
            }
        };
    }
    let Ok(Some(_lock)) = lock() else {
        eprintln!("arakun: another Arakun is updating already; try again in a moment");
        return 1;
    };
    println!(
        "arakun: looking for a new {}",
        match channel() {
            Channel::Stable => "release",
            Channel::Nightly => "nightly build",
        }
    );
    match check_locked() {
        Ok(Outcome::UpToDate) => {
            println!("arakun: {current} is the latest {}", latest_what());
            0
        }
        Ok(Outcome::Staged(tag, _)) if crate::control::app_running() => {
            // Only Linux and macOS can start Arakun again by themselves.
            let now = if cfg!(unix) {
                "arakun restart does that now"
            } else {
                "quit Arakun and open it again"
            };
            println!(
                "arakun: Arakun {tag} is downloaded; it is installed when Arakun quits ({now})"
            );
            0
        }
        Ok(Outcome::Staged(..)) => match apply_locked() {
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
        use Pre::Release;
        assert_eq!(Version::parse("v0.2.1"), Some(Version(0, 2, 1, Release)));
        assert_eq!(Version::parse("1.10.0"), Some(Version(1, 10, 0, Release)));
        assert!(Version::parse("v0.10.0") > Version::parse("v0.9.9"));
        assert_eq!(Version::parse("v1.0.0-rc.1"), None);
        assert_eq!(Version::parse("v1.0.0-nightly."), None);
        assert_eq!(Version::parse("v1.0.0-nightly.1a"), None);
        assert_eq!(Version::parse("v1.0"), None);
        assert_eq!(Version::parse("v1.0.0.1"), None);
        assert_eq!(Version::parse("latest"), None);
    }

    #[test]
    fn a_nightly_comes_between_its_release_and_the_one_before() {
        let v = |s| Version::parse(s).unwrap();
        let nightly = v("0.5.1-nightly.202611180300");
        assert_eq!(nightly, Version(0, 5, 1, Pre::Nightly(202611180300)));
        assert!(nightly.is_nightly());
        assert!(!v("0.5.1").is_nightly());
        assert!(v("0.5.0") < nightly);
        assert!(nightly < v("0.5.1"));
        assert!(nightly < v("0.5.1-nightly.202611190300"));
        assert!(v("0.6.0") > v("0.5.1-nightly.202612010300"));
    }

    #[test]
    fn the_nightly_key_signs_nightlies_only() {
        minisign_verify::PublicKey::decode(NIGHTLY_KEY).expect("assets/nightly-key.pub");
        assert!(!RELEASE_KEYS.contains(&NIGHTLY_KEY));
        let v = |s| Version::parse(s).unwrap();
        assert!(!keys_for(v("1.0.0")).contains(&NIGHTLY_KEY));
        assert!(keys_for(v("1.0.1-nightly.1")).contains(&NIGHTLY_KEY));
        assert!(keys_for(v("1.0.1-nightly.1")).contains(&RELEASE_KEYS[0]));
    }

    #[test]
    fn the_nightly_channel_takes_the_newest_build() {
        let list = r#"[
            {"tag_name": "0.6.1-nightly.202612020300", "draft": true, "prerelease": true},
            {"tag_name": "0.6.0", "draft": false, "prerelease": false},
            {"tag_name": "0.5.1-nightly.202612010300", "draft": false, "prerelease": true},
            {"tag_name": "v0.5.0", "draft": false, "prerelease": false},
            {"tag_name": "something-else", "draft": false, "prerelease": true}
        ]"#;
        assert_eq!(
            newest_listed(list).map(|(t, _)| t).as_deref(),
            Some("0.6.0")
        );
        let list = r#"[
            {"tag_name": "0.6.1-nightly.202612020300"},
            {"tag_name": "0.6.0"}
        ]"#;
        assert_eq!(
            newest_listed(list).map(|(t, _)| t).as_deref(),
            Some("0.6.1-nightly.202612020300")
        );
        assert_eq!(newest_listed("[]"), None);
        assert_eq!(
            newest_listed("{\"message\": \"API rate limit exceeded\"}"),
            None
        );
    }

    #[test]
    fn a_staged_nightly_waits_for_the_nightly_channel() {
        let v = |s| Version::parse(s).unwrap();
        let nightly = "0.5.1-nightly.202611180317";
        assert!(installable(nightly, Channel::Nightly, v("0.5.0")));
        // Staged before the switch to stable: dropped at quit.
        assert!(!installable(nightly, Channel::Stable, v("0.5.0")));
        assert!(installable("0.5.1", Channel::Stable, v("0.5.0")));
        assert!(installable("0.5.1", Channel::Nightly, v(nightly)));
        assert!(!installable("0.5.0", Channel::Nightly, v(nightly)));
        assert!(!installable("0.5.0", Channel::Stable, v("0.5.0")));
    }

    #[test]
    fn channels_read_back() {
        for c in [Channel::Stable, Channel::Nightly] {
            assert_eq!(Channel::parse(&format!("{}\n", c.name())), Some(c));
        }
        assert_eq!(Channel::parse("beta"), None);
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

    // A throwaway key made for these tests (`minisign -G -W`), and signatures
    // of SUMS by it, by another key, and in the legacy form (`minisign -l`).
    const TEST_KEY: &str = "\
untrusted comment: minisign public key BDEA25AA7F2AE71F
RWQf5yp/qiXqvS8wWa5ZLHg3DbJQZBv1YsB2UUc4qK6i2kH3ASM9j6Tj
";
    const SUMS: &str = "\
abc123  arakun-v9.0.0-linux-x86_64.tar.gz
def456  arakun-v9.0.0-windows-x86_64.zip
";
    const SIG: &str = "\
untrusted comment: signature from minisign secret key
RUQf5yp/qiXqvXf3pmw4RuYjZBBv2HO5948oEfytZmVWH22TkU6a9SdveG1vKAs29/HKmI3GuxVsCwVhEwh05vQZcBsj/WZojAg=
trusted comment: arakun v9.0.0 SHA256SUMS
Rbksjcbp31xsPsKp+lA5NMgWJqT21p41z2K0UtZ9fHnB3eWWxI1M9F3nDjrXanxJRpLvXbRKk5PtC3ZygV4tBA==
";
    const OTHER_KEY_SIG: &str = "\
untrusted comment: signature from minisign secret key
RUTYCaFiKOl0oU/p/pwRAiqOkH8qOiZezzN7lidJ2f20FDim8vCGf9ZPg/+UogrSPN1HiAKSLAdfdCu+q78GA93Ea2O8WjCX1AM=
trusted comment: arakun v9.0.0 SHA256SUMS
cPUi3Cp8VoMS3p9HainPG6Aodnk2bAfI1XcP1aGwXGWVJz7nju3KacxSrn49KeITtHWCt7jirwWW3ByS3aAbAw==
";
    const LEGACY_SIG: &str = "\
untrusted comment: signature from minisign secret key
RWQf5yp/qiXqvUYweyZrmNxRNsY/A9Wcrt9IqsQbecYk1kZdLaMsasdeHBya1z1KUdty+EsHFN7MMQfFei6T5IKs2vzlH8xhNwo=
trusted comment: legacy
E/PXP6NV7MfZ8NxQICz/mogdfOuIowQ5VLFE/4IC5gtsY2py3mp1d7ahKcI0zkxlgjsjCCpqCaO/ndQ+FsSPCQ==
";

    #[test]
    fn only_a_signature_by_the_key_is_accepted() {
        assert_eq!(signed(&[TEST_KEY], SUMS.as_bytes(), SIG), Ok(()));
        // Someone who changed a checksum but has no key.
        let changed = SUMS.replace("abc123", "abc124");
        assert!(signed(&[TEST_KEY], changed.as_bytes(), SIG).is_err());
        assert!(signed(&[TEST_KEY], SUMS.as_bytes(), OTHER_KEY_SIG).is_err());
        assert!(signed(&[TEST_KEY], SUMS.as_bytes(), LEGACY_SIG).is_err());
        // A release with no signature: GitHub's 404 page, or nothing.
        assert!(signed(&[TEST_KEY], SUMS.as_bytes(), "Not Found").is_err());
        assert!(signed(&[TEST_KEY], SUMS.as_bytes(), "").is_err());
        // The trusted comment is signed too.
        let comment = SIG.replace("arakun v9.0.0", "arakun v9.0.1");
        assert!(signed(&[TEST_KEY], SUMS.as_bytes(), &comment).is_err());
        // Either key will do, whichever comes first.
        assert_eq!(
            signed(&[RELEASE_KEYS[0], TEST_KEY], SUMS.as_bytes(), SIG),
            Ok(())
        );
        assert_eq!(
            signed(&[TEST_KEY, RELEASE_KEYS[1]], SUMS.as_bytes(), SIG),
            Ok(())
        );
        assert!(signed(&RELEASE_KEYS, SUMS.as_bytes(), SIG).is_err());
    }

    #[test]
    fn the_release_keys_are_the_same_everywhere() {
        let [main, backup] = RELEASE_KEYS.map(|key| {
            minisign_verify::PublicKey::decode(key).expect("assets/release-key*.pub");
            key.lines().nth(1).unwrap().trim()
        });
        assert_ne!(main, backup);
        for (script, text) in [
            (
                "tools/install-release.sh",
                include_str!("../tools/install-release.sh"),
            ),
            (
                "tools/install-release.ps1",
                include_str!("../tools/install-release.ps1"),
            ),
            ("README.md", include_str!("../README.md")),
        ] {
            for b64 in [main, backup] {
                assert!(
                    text.contains(b64),
                    "{script} does not carry the key {b64} from assets/"
                );
            }
        }
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
            assert_eq!(i.incoming, dir.join(".arakun.incoming"));
            assert_eq!(i.binary_in(&i.staged), i.staged);
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
            incoming: dir.join(".arakun.incoming"),
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

    // Linux swaps with one rename and has no .pdb to carry.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_pdb_follows_the_binary_into_place() {
        let dir = std::env::temp_dir().join(format!("arakun-update-pdb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let install = Install {
            target: dir.join("arakun.exe"),
            staged: dir.join(".arakun.exe.update"),
            incoming: dir.join(".arakun.exe.incoming"),
            old: dir.join(".arakun.exe.old"),
        };
        std::fs::write(&install.target, "old").unwrap();
        std::fs::write(dir.join("arakun.pdb"), "old symbols").unwrap();
        std::fs::write(&install.incoming, "new").unwrap();
        std::fs::write(pdb_beside(&install.incoming), "new symbols").unwrap();
        promote(&install).unwrap();
        // Staged, the running binary keeps its own .pdb.
        assert_eq!(
            std::fs::read_to_string(dir.join("arakun.pdb")).unwrap(),
            "old symbols"
        );
        swap(&install).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("arakun.pdb")).unwrap(),
            "new symbols"
        );
        assert!(!pdb_beside(&install.staged).exists());
        assert!(!pdb_beside(&install.incoming).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The island host keeps running the old .exe after an update, so the next
    /// update finds `.arakun.exe.old` still in use: Windows lets a running .exe
    /// be renamed but not deleted.
    #[cfg(windows)]
    #[test]
    fn a_swap_goes_on_while_the_old_exe_still_runs() {
        let dir = std::env::temp_dir().join(format!("arakun-update-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let install = Install::for_exe(&dir.join("arakun.exe")).unwrap();
        // A real program, so the file is locked the way a running arakun.exe is:
        // a copy of cmd.exe, which waits for commands on its stdin.
        let system = std::env::var_os("SystemRoot").unwrap();
        let cmd = std::path::Path::new(&system)
            .join("System32")
            .join("cmd.exe");
        std::fs::copy(&cmd, &install.target).unwrap();
        let mut running = std::process::Command::new(&install.target)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(&install.staged, "new").unwrap();
        swap(&install).unwrap();
        // The first update leaves the running .exe behind as `old`.
        assert!(install.old.exists());
        std::fs::write(&install.staged, "newer").unwrap();
        let second = swap(&install);
        let _ = running.kill();
        let _ = running.wait();
        second.unwrap();
        assert_eq!(std::fs::read_to_string(&install.target).unwrap(), "newer");
        assert!(!install.staged.exists());
        // Once the old one has stopped, nothing of it is left.
        remove_old(&install);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from("arakun.exe")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_old_files_the_updater_named_are_removed() {
        let dir = std::env::temp_dir().join(format!("arakun-update-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let install = Install {
            target: dir.join("arakun"),
            staged: dir.join(".arakun.update"),
            incoming: dir.join(".arakun.incoming"),
            old: dir.join(".arakun.old"),
        };
        std::fs::write(&install.old, "old").unwrap();
        std::fs::write(aside(&install.old), "older").unwrap();
        // Someone else's, with a name that starts the same way.
        std::fs::create_dir_all(dir.join(".arakun.old-backup")).unwrap();
        std::fs::write(dir.join(".arakun.old-backup").join("keep"), "keep").unwrap();
        std::fs::write(dir.join(".arakun.old-"), "keep").unwrap();
        remove_old(&install);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".arakun.old-", ".arakun.old-backup"]);
        assert!(dir.join(".arakun.old-backup").join("keep").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_staged_file_that_was_recorded_is_trusted() {
        if cfg!(target_os = "macos") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("arakun-update-intact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let install = Install::for_exe(&dir.join("arakun")).unwrap();
        std::fs::write(&install.incoming, "whole new binary").unwrap();
        promote(&install).unwrap();
        assert!(!install.incoming.exists());
        let mut saved = Saved {
            staged: Some("v9.0.0".into()),
            staged_sha256: Some(hash_file(&install.staged).unwrap()),
            notified: None,
        };
        assert!(install.staged_intact(&saved));
        // A newer download cut short, or a crash halfway through writing it.
        std::fs::write(&install.staged, "whole new bin").unwrap();
        assert!(!install.staged_intact(&saved));
        saved.staged_sha256 = None;
        assert!(!install.staged_intact(&saved));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_runtime_is_installed_once_and_never_mixed() {
        use RuntimeStep::*;
        assert_eq!(runtime_step(None, None), Nothing);
        assert_eq!(runtime_step(None, Some("a")), Nothing);
        assert_eq!(runtime_step(Some("a"), None), Install);
        assert_eq!(runtime_step(Some("a\n"), Some("a")), Nothing);
        assert_eq!(runtime_step(Some("b"), Some("a")), Mismatch);
        assert_eq!(runtime_step(Some("b"), Some("")), Mismatch);
    }

    /// A release with the browser, updating one from before it: the runtime
    /// goes where the loader looks, and the next update leaves it alone.
    #[test]
    fn a_first_runtime_goes_where_the_loader_looks() {
        if cfg!(target_os = "macos") {
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("arakun-update-runtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, exe_dir, app_dir) = (dir.join("x"), dir.join("bin"), dir.join("data"));
        let new_dir = if cfg!(windows) {
            root.clone()
        } else {
            root.join("cef")
        };
        std::fs::create_dir_all(new_dir.join("locales")).unwrap();
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(new_dir.join(LIBCEF), "lib").unwrap();
        std::fs::write(new_dir.join("archive.json"), "{\"name\": \"cef_156\"}").unwrap();
        std::fs::write(new_dir.join("locales").join("en-US.pak"), "pak").unwrap();
        update_runtime(&root, &exe_dir, &app_dir).unwrap();
        let installed = installed_runtime(&exe_dir, &app_dir);
        assert!(installed.join(LIBCEF).is_file());
        assert!(installed.join("locales").join("en-US.pak").is_file());
        if !cfg!(windows) {
            assert_eq!(installed, app_dir.join("cef"));
            // Moved, so stage it again for the next update.
            std::fs::create_dir_all(&new_dir).unwrap();
            std::fs::write(new_dir.join(LIBCEF), "lib").unwrap();
            std::fs::write(new_dir.join("archive.json"), "{\"name\": \"cef_156\"}").unwrap();
        }
        update_runtime(&root, &exe_dir, &app_dir).unwrap();
        std::fs::write(new_dir.join("archive.json"), "{\"name\": \"cef_157\"}").unwrap();
        assert!(update_runtime(&root, &exe_dir, &app_dir).is_err());
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
