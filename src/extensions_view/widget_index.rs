//! The community widget index the Marketplace lists: what the cloud's
//! `GET /widgets` answers, checked and ready to show. The format and the
//! rules are in `docs/superpowers/specs/2026-11-27-community-widgets-design.md`.
//!
//! Verified is decided here, never by the index: a version is Verified only
//! when Arakun's widget key signed its id, version, commit and file hash.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::widget::blocks::Block;
use crate::widget::manifest::{self, Manifest};
use crate::widget::sandbox;

/// The minisign keys that may sign a widget version as Verified. Empty until
/// Arakun makes its widget key: until then every widget is Not verified,
/// which is what a listing says when nobody has reviewed it.
pub(super) const WIDGET_KEYS: &[&str] = &[];

/// The index is small, but its versions carry three files each.
const MAX_INDEX: u64 = 16 * 1024 * 1024;

/// What Report offers, as the cloud takes it, with the words the menu shows.
pub(super) const REASONS: [(&str, &str); 5] = [
    ("misleading", "It does something it doesn't say"),
    ("harmful", "It is harmful or steals data"),
    ("broken", "It doesn't work"),
    ("copied", "It copies someone else's work"),
    ("other", "Something else"),
];

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Index {
    pub widgets: Vec<Listing>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Listing {
    pub id: String,
    pub author: String,
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub license: String,
    pub repository: String,
    #[serde(default)]
    pub installs: u64,
    /// The whole widget is pulled, with the reason.
    #[serde(default)]
    pub pulled: Option<String>,
    /// Newest first.
    pub versions: Vec<Version>,
    /// The files of the newest version that is not pulled.
    #[serde(default)]
    pub files: Option<IndexFiles>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Version {
    pub version: String,
    pub commit: String,
    /// When it was listed, in seconds since 1970.
    #[serde(default)]
    pub date: u64,
    pub hash: String,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub pulled: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct IndexFiles {
    pub version: String,
    #[serde(rename = "widget.toml")]
    pub manifest: String,
    #[serde(rename = "main.rhai")]
    pub script: String,
    #[serde(rename = "sample.json")]
    pub sample: String,
}

/// One widget as the Marketplace shows it: the listing, its newest version
/// with its needs, Verified or not, the preview and why it fits the project.
#[derive(Clone, Debug)]
pub(super) struct Card {
    pub id: String,
    pub name: String,
    pub author: String,
    pub about: String,
    pub license: String,
    pub repository: String,
    pub installs: u64,
    pub version: Version,
    pub manifest: Manifest,
    /// `widget.toml` as the index has it. An install refuses files whose
    /// manifest differs, so the needs the user allowed are the ones it gets.
    pub manifest_text: String,
    pub verified: bool,
    /// `view(sample.json)` in the size the card shows, or why it failed.
    pub preview: Result<Block, String>,
    /// What in the project makes it fit, such as "vercel.json in this project".
    pub fits: Option<String>,
}

impl Card {
    /// The source at the pinned commit.
    pub fn source_url(&self) -> String {
        format!("{}/tree/{}", self.repository, self.version.commit)
    }

    pub fn short_commit(&self) -> &str {
        &self.version.commit[..7.min(self.version.commit.len())]
    }

    /// The size a card or the details draw the preview in.
    pub fn preview_size(m: &Manifest) -> &str {
        ["wide", "small", "tab", "bar"]
            .into_iter()
            .find(|s| m.widget.sizes.iter().any(|w| w == s))
            .unwrap_or("wide")
    }
}

/// Reads the index from the cloud. Runs off the main thread.
pub(super) fn fetch() -> Result<Index, String> {
    let base = crate::cloud::base_url()?;
    let agent = crate::http::agent(Duration::from_secs(20));
    let reply = crate::http::json_call(
        agent,
        "GET",
        &format!("{base}/widgets"),
        None,
        None,
        MAX_INDEX,
    )?;
    if !reply.ok() {
        return Err(format!("Arakun's widget index answered {}", reply.status));
    }
    serde_json::from_str(&reply.text()).map_err(|e| format!("The widget index can't be read: {e}"))
}

/// Sends a report to Arakun. Runs off the main thread.
pub(super) fn report(id: &str, version: &str, reason: &str) -> Result<(), String> {
    let base = crate::cloud::base_url()?;
    let agent = crate::http::agent(Duration::from_secs(10));
    let body = serde_json::json!({ "id": id, "version": version, "reason": reason });
    let reply = crate::http::json_call(
        agent,
        "POST",
        &format!("{base}/widgets/report"),
        None,
        Some(body.to_string()),
        4 * 1024,
    )?;
    match reply.status {
        200..=299 => Ok(()),
        429 => Err("Too many reports from here. Try again in a minute.".into()),
        s => Err(format!("Arakun answered {s}: try again later")),
    }
}

/// Tells the index one more install. Best effort: the count is a guide.
pub(super) fn count_install(id: &str) {
    let Ok(base) = crate::cloud::base_url() else {
        return;
    };
    let agent = crate::http::agent(Duration::from_secs(10));
    let body = serde_json::json!({ "id": id });
    let _ = crate::http::json_call(
        agent,
        "POST",
        &format!("{base}/widgets/installed"),
        None,
        Some(body.to_string()),
        1024,
    );
}

/// What Arakun's signature covers: the version entry, with the needs inside
/// the file hash.
pub(super) fn signed_text(id: &str, v: &Version) -> String {
    format!(
        "arakun-widget-v1\n{id}\n{}\n{}\n{}\n",
        v.version, v.commit, v.hash
    )
}

/// Whether one of `keys` signed this version.
pub(super) fn verified(keys: &[&str], id: &str, v: &Version) -> bool {
    let Some(sig) = v.signature.as_deref() else {
        return false;
    };
    let Ok(sig) = minisign_verify::Signature::decode(sig) else {
        return false;
    };
    let text = signed_text(id, v);
    keys.iter().any(|k| {
        minisign_verify::PublicKey::decode(k)
            .is_ok_and(|k| k.verify(text.as_bytes(), &sig, false).is_ok())
    })
}

/// The cards to show: listings that are not pulled and whose newest version
/// is a widget this Arakun can draw, the ones that fit `project` first, then
/// the most installed. Runs the previews, so it runs off the main thread.
pub(super) fn cards(index: Index, keys: &[&str], project: Option<&Path>) -> Vec<Card> {
    let mut out: Vec<Card> = index
        .widgets
        .into_iter()
        .filter_map(|l| card(l, keys, project))
        .collect();
    out.sort_by(|a, b| {
        b.fits
            .is_some()
            .cmp(&a.fits.is_some())
            .then(b.installs.cmp(&a.installs))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

fn card(l: Listing, keys: &[&str], project: Option<&Path>) -> Option<Card> {
    if l.pulled.is_some() || manifest::check_id(&l.id).is_err() {
        return None;
    }
    let files = l.files?;
    let version = l
        .versions
        .into_iter()
        .find(|v| v.version == files.version && v.pulled.is_none())?;
    let m = Manifest::parse(&files.manifest).ok()?;
    // The listing must be the widget it names, and one this Arakun can draw.
    if m.widget.id != l.id
        || m.widget.version != version.version
        || m.widget.blocks > manifest::BLOCKS
    {
        return None;
    }
    let size = Card::preview_size(&m).to_string();
    let preview = sandbox::preview(m.clone(), &files.script, &files.sample, &size);
    let verified = verified(keys, &l.id, &version);
    let fits = project.and_then(|p| fits(&m, p));
    Some(Card {
        id: l.id,
        name: m.widget.name.clone(),
        author: l.author,
        about: if l.about.is_empty() {
            m.widget.about.clone()
        } else {
            l.about
        },
        license: if l.license.is_empty() {
            m.widget.license.clone()
        } else {
            l.license
        },
        repository: l.repository,
        installs: l.installs,
        version,
        manifest: m,
        manifest_text: files.manifest,
        verified,
        preview,
        fits,
    })
}

/// Why `m` fits the project: a file it reads is there. A file it names that
/// the project lacks would leave it with nothing to show.
pub(super) fn fits(m: &Manifest, project: &Path) -> Option<String> {
    let file = m
        .needs
        .file
        .iter()
        .find(|f| manifest::check_file(&f.path).is_ok() && project.join(&f.path).is_file())?;
    Some(format!("{} in this project", file.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "\
untrusted comment: minisign public key 0123456789ABCDEF
RWQBI0VniavN75m853g0WHYvUnj0SbQxJTXP/eHEqsbOi6yMJ8BHN4br
";
    // Made for these tests with a throwaway key, over
    // `signed_text("mrvn/deploy-board", 0.4.1 at COMMIT with HASH)`.
    const SIG: &str = "\
untrusted comment: signature from minisign secret key
RUQBI0VniavN72qgv0cB9as3yFYRoMFvWeyT5kNt0jwqoYVaUrRMZ6hRJh0oyG2M4FYZx3YLpbA1mYuFSOp7GdFakj0K4w4OEgw=
trusted comment: arakun widget mrvn/deploy-board 0.4.1
N5I6S2qV50EkVKohUW0XCYpc10pxqF2LVXIVdqqosvpaEg15F9rbsJUk+cKAEXk/Qz99opF6ExsdHqpcQFkOBQ==
";
    // The same text, signed with another key.
    const OTHER_SIG: &str = "\
untrusted comment: signature from minisign secret key
RUQBI0VniavN79n2nEEj95LpasUtrjz5hpvIvJPLZgiQyhmjyvH0ARvR/tLOh0dNEtjvZtqmxmIc8zSp3bdr+drjgf0jrZClYwU=
trusted comment: arakun widget mrvn/deploy-board 0.4.1
xUwITsPsJe2dibmkzpQgdf15RE7yDeZA8fNSi9znhlnTCc4V11nRmbf8ZzItPZgjgcphn7qmNYvy6e44NtNdDA==
";

    const MANIFEST: &str = r#"
[widget]
id = "mrvn/deploy-board"
name = "Deploy board"
about = "Vercel deploys and their checks, live"
version = "0.4.1"
license = "MIT"
blocks = 1
sizes = ["wide", "tab"]
every = "30s"

[[needs.net]]
host = "api.vercel.com"
why = "Deploys for your project"
every = "30s"

[[needs.file]]
path = "vercel.json"
why = "In this project only, to find the project id"
"#;

    const SCRIPT: &str = r#"
fn refresh(state) { state }
fn view(state, size) {
    list(state.deploys.map(|d| #{ dot: "success", title: d.branch, detail: d.state, time: d.at }))
}
"#;

    const SAMPLE: &str = r#"{"deploys":[{"branch":"main","state":"READY","at":"4m"}]}"#;

    fn commit() -> String {
        format!("9f2e1c4{}", "0".repeat(33))
    }

    fn version(signature: Option<&str>) -> Version {
        Version {
            version: "0.4.1".into(),
            commit: commit(),
            date: 0,
            hash: "a".repeat(64),
            signature: signature.map(str::to_string),
            pulled: None,
        }
    }

    fn listing(id: &str, installs: u64) -> Listing {
        Listing {
            id: id.into(),
            author: "mrvn".into(),
            about: String::new(),
            license: String::new(),
            repository: "https://github.com/mrvn/deploy-board".into(),
            installs,
            pulled: None,
            versions: vec![version(None)],
            files: Some(IndexFiles {
                version: "0.4.1".into(),
                manifest: MANIFEST.replace("mrvn/deploy-board", id),
                script: SCRIPT.into(),
                sample: SAMPLE.into(),
            }),
        }
    }

    #[test]
    fn only_arakuns_key_makes_a_version_verified() {
        let id = "mrvn/deploy-board";
        assert!(verified(&[KEY], id, &version(Some(SIG))));
        assert!(!verified(&[KEY], id, &version(Some(OTHER_SIG))));
        assert!(!verified(&[KEY], id, &version(None)));
        // A signature of one version says nothing of another commit.
        let mut moved = version(Some(SIG));
        moved.commit = "b".repeat(40);
        assert!(!verified(&[KEY], id, &moved));
        assert!(!verified(&[KEY], "mrvn/other", &version(Some(SIG))));
        // No key built in yet: nothing is Verified.
        assert!(!verified(WIDGET_KEYS, id, &version(Some(SIG))));
    }

    #[test]
    fn a_card_has_its_needs_and_a_preview_from_sample_data() {
        let cards = cards(
            Index {
                widgets: vec![listing("mrvn/deploy-board", 2100)],
            },
            &[],
            None,
        );
        let c = &cards[0];
        assert_eq!(c.about, "Vercel deploys and their checks, live");
        assert_eq!(c.license, "MIT");
        assert!(!c.verified);
        assert_eq!(c.manifest.needs.net[0].host, "api.vercel.com");
        assert!(matches!(&c.preview, Ok(Block::List(rows)) if rows[0].title == "main"));
        assert_eq!(
            c.source_url(),
            format!("https://github.com/mrvn/deploy-board/tree/{}", commit())
        );
        assert_eq!(c.short_commit(), "9f2e1c4");
    }

    #[test]
    fn pulled_mismatched_or_too_new_widgets_are_not_offered() {
        let mut pulled = listing("mrvn/a", 1);
        pulled.pulled = Some("malware".into());
        let mut pulled_version = listing("mrvn/b", 1);
        pulled_version.versions[0].pulled = Some("leaks tokens".into());
        // The index names one widget and serves another's files.
        let mut other = listing("mrvn/c", 1);
        other.id = "mrvn/d".into();
        let mut newer = listing("mrvn/e", 1);
        if let Some(f) = newer.files.as_mut() {
            f.manifest = f.manifest.replace("blocks = 1", "blocks = 2");
        }
        let index = Index {
            widgets: vec![pulled, pulled_version, other, newer, listing("mrvn/ok", 1)],
        };
        let ids: Vec<String> = cards(index, &[], None).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["mrvn/ok"]);
    }

    #[test]
    fn what_fits_the_project_comes_first() {
        let dir = std::env::temp_dir().join(format!("arakun-widget-fits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vercel.json"), "{}").unwrap();
        let mut popular = listing("ana/popular", 9000);
        if let Some(f) = popular.files.as_mut() {
            f.manifest = f.manifest.replace("vercel.json", "fly.toml");
        }
        let index = Index {
            widgets: vec![popular, listing("mrvn/deploy-board", 10)],
        };
        let cards = cards(index, &[], Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(cards[0].id, "mrvn/deploy-board");
        assert_eq!(
            cards[0].fits.as_deref(),
            Some("vercel.json in this project")
        );
        assert_eq!(cards[1].fits, None);
    }

    #[test]
    fn the_index_reads_as_the_cloud_writes_it() {
        let text = format!(
            r#"{{"widgets":[{{"id":"mrvn/deploy-board","name":"Deploy board","author":"mrvn",
            "about":"x","license":"MIT","repository":"https://github.com/mrvn/deploy-board",
            "installs":3,"pulled":null,
            "versions":[{{"version":"0.4.1","commit":"{}","date":1,"hash":"{}","signature":null,"pulled":null}}],
            "files":{{"version":"0.4.1","widget.toml":{},"main.rhai":{},"sample.json":{}}}}}]}}"#,
            commit(),
            "a".repeat(64),
            serde_json::to_string(MANIFEST).unwrap(),
            serde_json::to_string(SCRIPT).unwrap(),
            serde_json::to_string(SAMPLE).unwrap(),
        );
        let index: Index = serde_json::from_str(&text).unwrap();
        assert_eq!(cards(index, &[], None).len(), 1);
    }
}
