//! What keeps secrets out of @bot's reads and answers.
//!
//! The chat steers the run, and anyone in the channel can write in it. So a
//! member could ask the run to quote a key file, and the answer goes to the
//! whole channel. Two guards stand in the way, and neither depends on the
//! model following its prompt:
//!
//! - The run never sees the project itself, only a private copy of it
//!   ([`mirror`]) without secret-looking files
//!   ([`crate::secrets::secret_path`]) and without what git ignores. Claude Code reads only inside its working
//!   folder, so those files are not there to read. The keys and passwords
//!   in the files it does copy are replaced with `[redacted]` too, so the
//!   chat cannot have one posted spelled out, reversed or encoded, past the
//!   check on the answer.
//! - The answer goes through [`redact`] before it is posted, which replaces
//!   keys, tokens and passwords with `[redacted]`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::bot::Answer;

/// What a secret is replaced with in the answer.
pub const REDACTED: &str = crate::secrets::REDACTED;

/// How the folders of the run's copies start, in the temp folder.
pub const COPY_PREFIX: &str = "arakun-bot-";

/// A file larger than this is left out of the copy: it is rarely code, and a
/// few would fill the copy.
const FILE_BYTES: u64 = 1024 * 1024;
/// The copy stops growing here; the files past it are left out.
const TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// The copy holds at most this many files.
const FILES: usize = 50_000;

/// The copy of a project the run reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Mirror {
    /// Files copied.
    pub files: usize,
    /// Files left out for their size or past the copy's limits. Secret and
    /// ignored files are not counted: the run is not meant to know of them.
    pub too_big: usize,
    /// Files that could not be read or written, as when the temp folder is
    /// full.
    pub failed: usize,
    /// Files that are not text: [`redact`] cannot read them, so they are
    /// left out.
    pub binary: usize,
    /// Submodules and nested repositories, which git lists as one folder
    /// and the copy leaves out.
    pub nested: Vec<PathBuf>,
}

impl Mirror {
    /// What the answer says under "not checked" about what the copy left
    /// out, so the run is not taken to have read the whole project.
    pub fn notes(&self) -> Vec<String> {
        let files = |n: usize| {
            if n == 1 {
                "1 file of the project".to_string()
            } else {
                format!("{n} files of the project")
            }
        };
        let mut notes = Vec::new();
        if self.too_big > 0 {
            let verb = if self.too_big == 1 { "was" } else { "were" };
            notes.push(format!("{} {verb} too large to read.", files(self.too_big)));
        }
        if self.failed > 0 {
            notes.push(format!(
                "{} could not be copied to read.",
                files(self.failed)
            ));
        }
        if self.binary > 0 {
            let verb = if self.binary == 1 { "is" } else { "are" };
            notes.push(format!(
                "{} {verb} not text and {} not read.",
                files(self.binary),
                if self.binary == 1 { "was" } else { "were" }
            ));
        }
        if !self.nested.is_empty() {
            let names: Vec<String> = self
                .nested
                .iter()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .collect();
            notes.push(format!(
                "Submodules and nested repositories were not read: {}.",
                names.join(", ")
            ));
        }
        notes
    }
}

/// Copies `project` into the empty folder `into`, without secret-looking
/// files and, in a git checkout, without what git ignores. Symbolic links
/// are left out, so nothing in the copy leads outside it. Every file goes
/// through [`redact`] on the way: the chat can ask for a key in a form the
/// answer's check does not know, spelled out or reversed, so the run must
/// not see it at all. A file that is not text can't be checked that way (a
/// database or an archive can hold a key), so it is left out and counted.
/// Fails with "stopped" once `stop` is set.
pub fn mirror(project: &Path, into: &Path, stop: &AtomicBool) -> Result<Mirror, String> {
    let root = crate::platform::canonicalize(project)
        .map_err(|e| format!("could not open the project: {e}"))?;
    let mut out = Mirror::default();
    let files = match git_files(&root) {
        Some(files) => files,
        None => {
            let (files, past) = walk_files(&root, stop);
            out.too_big += past;
            files
        }
    };
    let mut total = 0u64;
    for rel in files {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        if crate::secrets::secret_path(&rel) {
            continue;
        }
        let from = root.join(&rel);
        // Not following links: a link in the project can point anywhere.
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if meta.is_dir() {
            // Git lists a submodule or a nested repository as its folder.
            out.nested.push(rel);
            continue;
        }
        if !meta.file_type().is_file() {
            continue;
        }
        if meta.len() > FILE_BYTES || total + meta.len() > TOTAL_BYTES || out.files >= FILES {
            out.too_big += 1;
            continue;
        }
        match copy_redacted(&from, &into.join(&rel)) {
            Ok(true) => {
                total += meta.len();
                out.files += 1;
            }
            Ok(false) => out.binary += 1,
            Err(_) => out.failed += 1,
        }
    }
    Ok(out)
}

/// Copies one file with its secrets replaced, or answers `false` and
/// copies nothing when it is not text.
fn copy_redacted(from: &Path, to: &Path) -> std::io::Result<bool> {
    let bytes = std::fs::read(from)?;
    let Some(text) = text_of(&bytes) else {
        return Ok(false);
    };
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(to, redact(&text))?;
    Ok(true)
}

/// The text of a file: UTF-8, or UTF-16 with a byte order mark, as Windows
/// tools write settings. The copy of the second is UTF-8. `None` for
/// anything else, and for text with a NUL in it, which is a binary file
/// that happens to decode.
fn text_of(bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| {
        let (pairs, odd) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| unit(*c)).collect();
        odd.is_empty()
            .then(|| String::from_utf16(&units).ok())
            .flatten()
    };
    let text: std::borrow::Cow<'_, str> = match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes)?.into(),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes)?.into(),
        _ => std::str::from_utf8(bytes).ok()?.into(),
    };
    (!text.contains('\0')).then_some(text)
}

/// Removes copies a run left behind when Arakun quit in the middle of it.
/// Only those a day old: another Arakun may be running one now.
pub fn sweep_copies() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for e in entries.flatten() {
        let old = e
            .metadata()
            .ok()
            .filter(|m| m.is_dir())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old && e.file_name().to_string_lossy().starts_with(COPY_PREFIX) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The files of a git checkout, relative to `root`: tracked and untracked,
/// minus everything git ignores, tracked or not. `None` outside git.
fn git_files(root: &Path) -> Option<Vec<PathBuf>> {
    let list = |extra: &[&str]| -> Option<Vec<PathBuf>> {
        let mut args = vec!["ls-files", "-z", "--exclude-standard"];
        args.extend_from_slice(extra);
        // `git::git` drops a GIT_DIR inherited from whatever started Arakun,
        // which would list another repository's files and ignore rules.
        let out = crate::platform::output_within(
            crate::git::git(root).args(&args),
            None,
            crate::platform::GIT_LOCAL,
        )
        .ok()
        .filter(|o| o.status.success())?;
        Some(split_z(&out.stdout))
    };
    let all = list(&["--cached", "--others"])?;
    // A tracked file can match an ignore rule too; leave it out as well.
    let ignored: HashSet<PathBuf> = list(&["--cached", "--ignored"])?.into_iter().collect();
    Some(all.into_iter().filter(|p| !ignored.contains(p)).collect())
}

fn split_z(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
        .filter(|p| p.components().all(|c| matches!(c, Component::Normal(_))))
        .collect()
}

/// The files of a folder outside git, relative to it, up to [`FILES`], and
/// how many more there are. Hidden files are left out, and `.gitignore`
/// files are still followed.
fn walk_files(root: &Path, stop: &AtomicBool) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    let mut past = 0;
    let walk = ignore::WalkBuilder::new(root)
        .require_git(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .filter(|p| !crate::secrets::secret_path(p));
    for rel in walk {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if files.len() < FILES {
            files.push(rel);
        } else {
            past += 1;
        }
    }
    (files, past)
}

// ---- the answer ------------------------------------------------------------------

/// Replaces keys, tokens, passwords and credentials in `text` with
/// [`REDACTED`]. Code and prose around them stay as they are.
pub fn redact(text: &str) -> String {
    crate::secrets::redact(text, REDACTED)
}

/// Redacts every text in the answer, and takes the copy's folder out of
/// paths so sources read as paths in the project.
pub fn clean_answer(a: &mut Answer, copy: Option<&Path>) {
    let prefix = copy.map(|p| {
        let mut s = p.to_string_lossy().into_owned();
        s.push(std::path::MAIN_SEPARATOR);
        s
    });
    let clean = |s: &mut String| {
        if let Some(prefix) = &prefix {
            *s = s.replace(prefix.as_str(), "");
        }
        *s = redact(s);
    };
    for idea in &mut a.ideas {
        clean(&mut idea.label);
        clean(&mut idea.title);
        clean(&mut idea.reason);
    }
    for v in &mut a.verified {
        clean(&mut v.text);
        clean(&mut v.source);
    }
    a.assumed.iter_mut().for_each(clean);
    a.unchecked.iter_mut().for_each(clean);
    clean(&mut a.decision);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = crate::git::git(dir)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn the_copy_leaves_out_secrets_ignored_files_and_links() {
        let base = std::env::temp_dir().join(format!(
            "arakun-bot-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let (project, copy) = (base.join("project"), base.join("copy"));
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join(".ssh")).unwrap();
        std::fs::create_dir_all(&copy).unwrap();
        let write = |p: &str, s: &str| std::fs::write(project.join(p), s).unwrap();
        write("src/main.rs", "fn main() {}");
        write("notes.md", "untracked but not ignored");
        write(".gitignore", "secrets.txt\nbuild/\n");
        write("secrets.txt", "ignored");
        write("tracked-ignored.txt", "tracked, then ignored");
        write(".env", "API_KEY=1");
        write("server.pem", "key");
        write(".ssh/id_rsa", "key");
        std::fs::create_dir_all(project.join("build")).unwrap();
        write("build/out.txt", "ignored dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", project.join("link")).unwrap();
        write("settings.py", "SECRET_KEY = 'django-insecure-abc123'\n");
        std::fs::create_dir_all(project.join("vendor/lib")).unwrap();
        std::fs::write(project.join("vendor/lib/lib.rs"), "").unwrap();
        git(&project.join("vendor/lib"), &["init", "-q"]);
        git(&project.join("vendor/lib"), &["add", "lib.rs"]);
        git(
            &project.join("vendor/lib"),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "x",
            ],
        );
        git(&project, &["init", "-q"]);
        git(
            &project,
            &[
                "add",
                "-f",
                "src/main.rs",
                "tracked-ignored.txt",
                ".env",
                "vendor/lib",
            ],
        );
        std::fs::write(
            project.join(".gitignore"),
            "secrets.txt\nbuild/\ntracked-ignored.txt\n",
        )
        .unwrap();

        let m = mirror(&project, &copy, &AtomicBool::new(false)).unwrap();
        let mut got: Vec<String> = ignore::WalkBuilder::new(&copy)
            .standard_filters(false)
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(|e| {
                e.path()
                    .strip_prefix(&copy)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            [".gitignore", "notes.md", "settings.py", "src/main.rs"]
        );
        assert_eq!(m.files, 4);
        assert_eq!(m.nested, [PathBuf::from("vendor/lib")]);
        assert_eq!(
            std::fs::read_to_string(copy.join("settings.py")).unwrap(),
            "SECRET_KEY = '[redacted]'\n"
        );

        let stopped = base.join("stopped");
        std::fs::create_dir_all(&stopped).unwrap();
        assert_eq!(
            mirror(&project, &stopped, &AtomicBool::new(true)),
            Err("stopped".into())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_answer_names_what_the_copy_left_out() {
        let m = Mirror {
            files: 10,
            too_big: 1,
            failed: 3,
            binary: 2,
            nested: vec![PathBuf::from("vendor/lib")],
        };
        assert_eq!(
            m.notes(),
            [
                "1 file of the project was too large to read.",
                "3 files of the project could not be copied to read.",
                "2 files of the project are not text and were not read.",
                "Submodules and nested repositories were not read: vendor/lib.",
            ]
        );
        assert!(Mirror::default().notes().is_empty());
    }

    #[test]
    fn a_file_that_is_not_text_is_left_out_of_the_copy() {
        let base = std::env::temp_dir().join(format!("arakun-guard-binary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (project, copy) = (base.join("project"), base.join("copy"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(project.join("notes.md"), "password=hunter22\n").unwrap();
        // A database with a key in it: no pattern would find it there.
        let mut db = b"SQLite format 3\0".to_vec();
        db.extend_from_slice(&[0xFF, 0x00, 0x80]);
        db.extend_from_slice(b"password=hunter22");
        std::fs::write(project.join("app.db"), &db).unwrap();
        // Text with a NUL decodes as UTF-8 and is still not text.
        std::fs::write(project.join("blob.bin"), b"abc\0password=hunter22").unwrap();
        // Settings as Windows tools write them: UTF-16 with a mark.
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "password=hunter22\r\n".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(project.join("app.config"), &utf16).unwrap();

        let m = mirror(&project, &copy, &AtomicBool::new(false)).unwrap();
        assert_eq!((m.files, m.binary, m.failed), (2, 2, 0));
        assert!(!copy.join("app.db").exists());
        assert!(!copy.join("blob.bin").exists());
        assert_eq!(
            std::fs::read_to_string(copy.join("app.config")).unwrap(),
            "password=[redacted]\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("notes.md")).unwrap(),
            "password=[redacted]\n"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cleans_every_text_in_the_answer() {
        let mut a = super::super::bot::parse_answer(
            r#"{"ideas":[{"label":"A","title":"Keep it","verdict":"works","reason":"the key is sk-ant-0123456789abcdefghijkl","cites":[1]}],
               "verified":[{"text":"password=hunter22 is set","source":"/tmp/arakun-bot-x/config/app.toml"}],
               "assumed":[],"unchecked":[],"decision":"Rotate ghp_abcdefghijklmnopqrstuvwxyz0123456789."}"#,
            1,
        )
        .unwrap();
        clean_answer(&mut a, Some(Path::new("/tmp/arakun-bot-x")));
        assert_eq!(a.ideas[0].reason, "the key is [redacted]");
        assert_eq!(a.verified[0].text, "password=[redacted] is set");
        assert_eq!(a.verified[0].source, "config/app.toml");
        assert_eq!(a.decision, "Rotate [redacted].");
    }
}
