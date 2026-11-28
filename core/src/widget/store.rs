//! Installed widgets: their files under `widgets/<author>/<name>/<version>/`
//! and what the user chose in `widgets/widgets.toml`, where it shows, whether
//! it is paused and what it was allowed. Tokens are not here: they are in the
//! keyring under the names [`token_account`] gives.
//!
//! An install is pinned to a commit and a hash of the files, and the files
//! are hashed again every time they load, so neither a changed repository
//! nor a changed file on disk can change what runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::manifest::{self, Files, Grant, Manifest, OPTIONAL, REQUIRED};
use super::sandbox;

/// The settings file in the widgets folder.
pub const SETTINGS: &str = "widgets.toml";

/// Where a widget shows. The user picks it on install and can change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    /// The Widgets segment of the Workspace panel, `small` or `wide`.
    Workspace,
    /// A tab of its own.
    Tab,
    /// One line in a bar.
    Bar,
}

impl Place {
    /// The sizes a widget may draw in here, best first.
    pub fn sizes(self) -> &'static [&'static str] {
        match self {
            Place::Workspace => &["wide", "small"],
            Place::Tab => &["tab"],
            Place::Bar => &["bar"],
        }
    }

    /// What the Set for cell says under who it is for.
    pub fn label(self) -> &'static str {
        match self {
            Place::Workspace => "Workspace panel",
            Place::Tab => "Its own tab",
            Place::Bar => "Bar",
        }
    }

    /// The size `m` draws in here, if it draws in this place at all.
    pub fn size_for(self, m: &Manifest) -> Option<&'static str> {
        self.sizes()
            .iter()
            .find(|s| m.widget.sizes.iter().any(|w| w == *s))
            .copied()
    }
}

/// Where a version came from: the index entry it was pinned to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub repo: String,
    pub commit: String,
    /// [`manifest::hash`] of the files, as the index has it.
    pub hash: String,
    /// The index's signature checked out for this version.
    #[serde(default)]
    pub verified: bool,
}

/// A newer version that asks for more than the user allowed. Its files are
/// on disk; it runs once the user allows [`Waiting::asks`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub version: String,
    pub source: Source,
    pub grant: Grant,
    /// What it asks for beyond the grant, in plain lines.
    pub asks: Vec<String>,
}

/// One installed widget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: Source,
    pub place: Place,
    pub size: String,
    /// The project it shows in and reads from.
    pub project: PathBuf,
    #[serde(default)]
    pub paused: bool,
    pub grant: Grant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Waiting>,
    /// Pulled from the index, with the reason. It does not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulled: Option<String>,
}

impl Record {
    pub fn author(&self) -> &str {
        self.id.split_once('/').map_or("", |(a, _)| a)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    widget: Vec<Record>,
}

/// How an offered update went.
#[derive(Debug, PartialEq)]
pub enum Updated {
    /// It asks for nothing new and runs now.
    Installed,
    /// It asks for more: Update available, and the old version keeps running.
    Waiting(Vec<String>),
    /// That version is the one installed.
    Same,
}

pub struct Store {
    dir: PathBuf,
    pub widgets: Vec<Record>,
}

impl Store {
    /// The widgets folder in the data folder.
    pub fn dir() -> PathBuf {
        crate::paths::store(crate::paths::WIDGETS)
    }

    /// Reads `widgets.toml` in `dir`. A missing file is no widgets; a broken
    /// one is an error, so a save can't overwrite what the user set.
    pub fn open(dir: &Path) -> Result<Store, String> {
        let path = dir.join(SETTINGS);
        let saved: Saved = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Store {
            dir: dir.to_path_buf(),
            widgets: saved.widget,
        })
    }

    pub fn save(&self) -> Result<(), String> {
        let text = toml::to_string_pretty(&Saved {
            widget: self.widgets.clone(),
        })
        .map_err(|e| e.to_string())?;
        crate::durable::write_private(&self.dir.join(SETTINGS), text.as_bytes())
            .map_err(|e| e.to_string())
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.widgets.iter().find(|r| r.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Result<&mut Record, String> {
        self.widgets
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("{id} is not installed"))
    }

    fn version_dir(&self, id: &str, version: &str) -> PathBuf {
        let (author, name) = id.split_once('/').unwrap_or((id, ""));
        self.dir.join(author).join(name).join(version)
    }

    /// Installs `files` after the user allowed its needs ("Allow and add").
    /// The grant is exactly what the manifest asks for.
    pub fn install(
        &mut self,
        source: Source,
        files: &Files,
        place: Place,
        project: PathBuf,
    ) -> Result<&Record, String> {
        let m = check(&source, files)?;
        if self.get(&m.widget.id).is_some() {
            return Err(format!("{} is already installed", m.widget.name));
        }
        let size = place
            .size_for(&m)
            .ok_or_else(|| format!("{} can't show in the {}", m.widget.name, place.label()))?;
        self.write_files(&m, files)?;
        self.widgets.push(Record {
            id: m.widget.id.clone(),
            name: m.widget.name.clone(),
            version: m.widget.version.clone(),
            source,
            place,
            size: size.into(),
            project,
            paused: false,
            grant: Grant::of(&m),
            waiting: None,
            pulled: None,
        });
        self.save()?;
        Ok(self.widgets.last().expect("just pushed"))
    }

    /// Offers a newer version. One that asks for nothing new replaces the
    /// old at once; one that asks for more waits for [`Store::allow_update`].
    pub fn update(&mut self, source: Source, files: &Files) -> Result<Updated, String> {
        let m = check(&source, files)?;
        let id = m.widget.id.clone();
        let rec = self
            .get(&id)
            .ok_or_else(|| format!("{id} is not installed"))?;
        if rec.version == m.widget.version {
            // A version is pinned once: the same number with other files is
            // a swap, not an update.
            return if rec.source.hash == source.hash {
                Ok(Updated::Same)
            } else {
                Err(format!(
                    "{} {} changed after it was published",
                    rec.name, rec.version
                ))
            };
        }
        let next = Grant::of(&m);
        let asks = rec.grant.lacks(&next);
        // A place the new version no longer draws in is a change too.
        let size = rec.place.size_for(&m);
        let old_version = rec.version.clone();
        self.write_files(&m, files)?;
        let rec = self.get_mut(&id)?;
        if asks.is_empty() && size.is_some() {
            rec.version = m.widget.version.clone();
            rec.source = source;
            rec.grant = next;
            rec.size = size.unwrap_or_default().into();
            rec.waiting = None;
            self.save()?;
            self.remove_version(&id, &old_version);
            return Ok(Updated::Installed);
        }
        let mut asks = asks;
        if size.is_none() {
            asks.push(format!("Move out of the {}", rec.place.label()));
        }
        if let Some(w) = rec.waiting.replace(Waiting {
            version: m.widget.version.clone(),
            source,
            grant: next,
            asks: asks.clone(),
        }) && w.version != m.widget.version
            && w.version != old_version
        {
            self.remove_version(&id, &w.version);
        }
        self.save()?;
        Ok(Updated::Waiting(asks))
    }

    /// The user allowed what the waiting update asks for (the Needs step
    /// again). `place` is where it shows now, if it moved.
    pub fn allow_update(&mut self, id: &str, place: Option<Place>) -> Result<(), String> {
        let rec = self
            .get(id)
            .ok_or_else(|| format!("{id} is not installed"))?;
        let w = rec
            .waiting
            .clone()
            .ok_or_else(|| format!("{} has no update waiting", rec.name))?;
        let place = place.unwrap_or(rec.place);
        let (m, _) = self.load_version(id, &w.version, &w.source.hash)?;
        let size = place
            .size_for(&m)
            .ok_or_else(|| format!("{} can't show in the {}", m.widget.name, place.label()))?;
        let old = rec.version.clone();
        let rec = self.get_mut(id)?;
        rec.version = w.version;
        rec.source = w.source;
        rec.grant = w.grant;
        rec.place = place;
        rec.size = size.into();
        rec.waiting = None;
        self.save()?;
        self.remove_version(id, &old);
        Ok(())
    }

    pub fn set_paused(&mut self, id: &str, paused: bool) -> Result<(), String> {
        self.get_mut(id)?.paused = paused;
        self.save()
    }

    /// Moves it to another place, from the ⋯ menu.
    pub fn move_to(&mut self, id: &str, place: Place) -> Result<(), String> {
        let rec = self
            .get(id)
            .ok_or_else(|| format!("{id} is not installed"))?;
        let (m, _) = self.load(id)?;
        let size = place
            .size_for(&m)
            .ok_or_else(|| format!("{} can't show in the {}", rec.name, place.label()))?;
        let rec = self.get_mut(id)?;
        rec.place = place;
        rec.size = size.into();
        self.save()
    }

    /// The index pulled it: it stops, its files go, and the reason stays on
    /// its row until the user removes it. Returns the secrets it used, so the
    /// app can offer to forget them.
    pub fn pull(&mut self, id: &str, reason: &str) -> Result<Vec<String>, String> {
        let rec = self.get_mut(id)?;
        rec.pulled = Some(reason.to_string());
        rec.waiting = None;
        let secrets = rec
            .grant
            .needs
            .secret
            .iter()
            .map(|s| s.name.clone())
            .collect();
        self.save()?;
        self.remove_files(id);
        Ok(secrets)
    }

    /// Removes it and its files. Returns the secrets it used, so the app can
    /// offer to forget them.
    pub fn remove(&mut self, id: &str) -> Result<Vec<String>, String> {
        let i = self
            .widgets
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| format!("{id} is not installed"))?;
        let rec = self.widgets.remove(i);
        self.save()?;
        self.remove_files(id);
        Ok(rec
            .grant
            .needs
            .secret
            .iter()
            .map(|s| s.name.clone())
            .collect())
    }

    /// The installed version's manifest and script, hashed again.
    pub fn load(&self, id: &str) -> Result<(Manifest, String), String> {
        let rec = self
            .get(id)
            .ok_or_else(|| format!("{id} is not installed"))?;
        if let Some(why) = &rec.pulled {
            return Err(format!("Pulled: {why}"));
        }
        self.load_version(id, &rec.version, &rec.source.hash)
    }

    fn load_version(
        &self,
        id: &str,
        version: &str,
        hash: &str,
    ) -> Result<(Manifest, String), String> {
        let files = read_dir(&self.version_dir(id, version))?;
        if manifest::hash(&files) != hash {
            return Err("Its files changed on disk since it was installed".into());
        }
        let m = Manifest::parse(&text(&files, "widget.toml")?)?;
        Ok((m, text(&files, "main.rhai")?))
    }

    fn write_files(&self, m: &Manifest, files: &Files) -> Result<(), String> {
        let dir = self.version_dir(&m.widget.id, &m.widget.version);
        crate::durable::create_private_dir(&dir).map_err(|e| e.to_string())?;
        for (name, bytes) in files {
            crate::durable::write_private(&dir.join(name), bytes)
                .map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    }

    fn remove_version(&self, id: &str, version: &str) {
        let _ = std::fs::remove_dir_all(self.version_dir(id, version));
    }

    fn remove_files(&self, id: &str) {
        if let Some((author, name)) = id.split_once('/') {
            let _ = std::fs::remove_dir_all(self.dir.join(author).join(name));
            // The author's folder goes with their last widget.
            let _ = std::fs::remove_dir(self.dir.join(author));
        }
    }
}

/// The keyring account a widget's token is kept under.
pub fn token_account(id: &str, secret: &str) -> String {
    format!("widget:{id}:{secret}")
}

/// Everything an install checks before a file is written: the set of files,
/// the pinned hash, the manifest, and that the script compiles and draws its
/// sample.
fn check(source: &Source, files: &Files) -> Result<Manifest, String> {
    manifest::check_files(files)?;
    if manifest::hash(files) != source.hash {
        return Err("The files are not the ones the index pinned".into());
    }
    let m = Manifest::parse(&text(files, "widget.toml")?)?;
    let main = text(files, "main.rhai")?;
    let sample = text(files, "sample.json")?;
    for size in &m.widget.sizes {
        sandbox::preview(m.clone(), &main, &sample, size)
            .map_err(|e| format!("Its preview fails at {size}: {e}"))?;
    }
    Ok(m)
}

fn text(files: &Files, name: &str) -> Result<String, String> {
    let bytes = files
        .get(name)
        .ok_or_else(|| format!("{name} is missing"))?;
    String::from_utf8(bytes.clone()).map_err(|_| format!("{name} is not UTF-8"))
}

/// A widget's files from a folder: an author trying theirs, or an installed
/// version. Links and folders in it are refused.
pub fn read_dir(dir: &Path) -> Result<Files, String> {
    let mut files = Files::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // A repository's own metadata is not part of the widget.
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if !kind.is_file() {
            return Err(format!("{name} is not a plain file"));
        }
        if !REQUIRED.contains(&name.as_str()) && !OPTIONAL.contains(&name.as_str()) {
            return Err(format!("{name} is not allowed in a widget"));
        }
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if meta.len() > manifest::MAX_FILES as u64 {
            return Err(format!("{name} is larger than 512 KB"));
        }
        let bytes = std::fs::read(entry.path()).map_err(|e| format!("{name}: {e}"))?;
        files.insert(name, bytes);
    }
    manifest::check_files(&files)?;
    Ok(files)
}

/// Downloads a version's files from GitHub at its commit. The caller checks
/// them against the index's hash ([`Store::install`] does).
pub fn download(repo: &str, commit: &str) -> Result<Files, String> {
    let slug = repo
        .strip_prefix("https://github.com/")
        .map(|s| s.trim_end_matches('/').trim_end_matches(".git"))
        .filter(|s| {
            let parts: Vec<&str> = s.split('/').collect();
            parts.len() == 2
                && parts.iter().all(|p| {
                    !p.is_empty()
                        && !p.starts_with('.')
                        && p.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                })
        })
        .ok_or_else(|| format!("{repo} is not a GitHub repository"))?;
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{commit} is not a full commit"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into();
    let mut files = Files::new();
    let mut size = 0;
    for name in REQUIRED.iter().chain(OPTIONAL.iter()) {
        let url = format!("https://raw.githubusercontent.com/{slug}/{commit}/{name}");
        let mut resp = agent.get(&url).call().map_err(|e| format!("{name}: {e}"))?;
        match resp.status().as_u16() {
            200 => {}
            404 if OPTIONAL.contains(name) => continue,
            s => return Err(format!("{name}: GitHub answered {s}")),
        }
        let bytes = resp
            .body_mut()
            .with_config()
            .limit((manifest::MAX_FILES - size) as u64 + 1)
            .read_to_vec()
            .map_err(|e| format!("{name}: {e}"))?;
        size += bytes.len();
        if size > manifest::MAX_FILES {
            return Err("The files take more than 512 KB".into());
        }
        files.insert(name.to_string(), bytes);
    }
    Ok(files)
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::manifest::DEPLOY_BOARD;
    use super::super::sandbox::tests::MAIN;
    use super::*;

    pub fn files(manifest: &str) -> Files {
        let mut f = Files::new();
        f.insert("widget.toml".into(), manifest.as_bytes().to_vec());
        f.insert("main.rhai".into(), MAIN.as_bytes().to_vec());
        f.insert(
            "sample.json".into(),
            br#"{"deploys":[{"branch":"main","state":"READY","at":"2m"}]}"#.to_vec(),
        );
        f
    }

    pub fn source(files: &Files) -> Source {
        Source {
            repo: "https://github.com/mrvn/deploy-board".into(),
            commit: "a".repeat(40),
            hash: manifest::hash(files),
            verified: false,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-widgets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn install_load_and_remove() {
        let dir = temp("install");
        let mut store = Store::open(&dir).unwrap();
        let f = files(DEPLOY_BOARD);
        let rec = store
            .install(source(&f), &f, Place::Workspace, "/p".into())
            .unwrap();
        assert_eq!(rec.size, "wide");
        assert!(dir.join("mrvn/deploy-board/0.4.1/main.rhai").is_file());

        let store = Store::open(&dir).unwrap();
        let (m, main) = store.load("mrvn/deploy-board").unwrap();
        assert_eq!(m.widget.version, "0.4.1");
        assert_eq!(main, MAIN);

        // A file changed on disk no longer loads.
        std::fs::write(dir.join("mrvn/deploy-board/0.4.1/main.rhai"), "fn x() {}").unwrap();
        assert!(
            store
                .load("mrvn/deploy-board")
                .unwrap_err()
                .contains("changed")
        );

        let mut store = store;
        assert_eq!(store.remove("mrvn/deploy-board").unwrap(), ["vercel"]);
        assert!(!dir.join("mrvn").exists());
        assert!(Store::open(&dir).unwrap().widgets.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn install_refuses_what_the_index_did_not_pin() {
        let dir = temp("pin");
        let mut store = Store::open(&dir).unwrap();
        let f = files(DEPLOY_BOARD);
        let mut s = source(&f);
        s.hash = "0".repeat(64);
        assert!(
            store
                .install(s, &f, Place::Tab, "/p".into())
                .unwrap_err()
                .contains("pinned")
        );
        // It draws in wide and tab, not a bar.
        let s = source(&f);
        assert!(store.install(s, &f, Place::Bar, "/p".into()).is_err());
        assert!(!dir.join("mrvn").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_update_that_asks_for_more_waits() {
        let dir = temp("update");
        let mut store = Store::open(&dir).unwrap();
        let f = files(DEPLOY_BOARD);
        store
            .install(source(&f), &f, Place::Workspace, "/p".into())
            .unwrap();

        // Same needs: installs by itself.
        let same = files(&DEPLOY_BOARD.replace("0.4.1", "0.4.2"));
        assert_eq!(store.update(source(&same), &same), Ok(Updated::Installed));
        assert_eq!(store.get("mrvn/deploy-board").unwrap().version, "0.4.2");
        assert!(!dir.join("mrvn/deploy-board/0.4.1").exists());

        // A new host: waits, and the old one keeps running.
        let more = files(&DEPLOY_BOARD.replace("0.4.1", "0.5.0").replace(
            "[[needs.file]]",
            "[[needs.net]]\nhost = \"api.sentry.io\"\nwhy = \"Errors\"\nevery = \"60s\"\n\n[[needs.file]]",
        ));
        assert_eq!(
            store.update(source(&more), &more),
            Ok(Updated::Waiting(vec!["Reach api.sentry.io".into()]))
        );
        let rec = store.get("mrvn/deploy-board").unwrap();
        assert_eq!(rec.version, "0.4.2");
        assert_eq!(rec.waiting.as_ref().unwrap().version, "0.5.0");
        assert!(store.load("mrvn/deploy-board").is_ok());

        store.allow_update("mrvn/deploy-board", None).unwrap();
        let rec = store.get("mrvn/deploy-board").unwrap();
        assert_eq!(rec.version, "0.5.0");
        assert!(rec.grant.needs.net("api.sentry.io").is_some());
        assert!(rec.waiting.is_none());
        assert!(!dir.join("mrvn/deploy-board/0.4.2").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pulled_widget_does_not_load() {
        let dir = temp("pull");
        let mut store = Store::open(&dir).unwrap();
        let f = files(DEPLOY_BOARD);
        store
            .install(source(&f), &f, Place::Tab, "/p".into())
            .unwrap();
        assert_eq!(
            store.pull("mrvn/deploy-board", "Sent tokens away").unwrap(),
            ["vercel"]
        );
        assert_eq!(
            store.load("mrvn/deploy-board").unwrap_err(),
            "Pulled: Sent tokens away"
        );
        assert!(!dir.join("mrvn/deploy-board").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_with_extra_files_is_refused() {
        let dir = temp("folder");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files(DEPLOY_BOARD) {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        assert!(read_dir(&dir).is_ok());
        std::fs::write(dir.join("helper.so"), b"\x7fELF").unwrap();
        assert!(read_dir(&dir).unwrap_err().contains("helper.so"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn downloads_only_from_github_at_a_full_commit() {
        assert!(download("https://evil.example.com/a/b", &"a".repeat(40)).is_err());
        assert!(download("https://github.com/a/b", "main").is_err());
        assert!(download("https://github.com/../b", &"a".repeat(40)).is_err());
    }
}
