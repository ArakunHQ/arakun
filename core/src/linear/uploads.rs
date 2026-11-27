//! The files people upload to an issue, such as screenshots, for a delegated
//! session. Linear's links to them need Linear's sign-in, so the agent can't
//! open them. They are downloaded with the person's key into a folder in the
//! environment that git ignores, and their paths take the links' place.

use std::path::{Path, PathBuf};

use super::issues::Detail;

/// Where Linear keeps uploads. The key is only ever sent to this host.
const HOST: &str = "https://uploads.linear.app/";

/// The folder in the environment, ignored by git through its own `.gitignore`.
pub const FOLDER: &str = ".arakun-linear";

/// The most files downloaded for one issue.
pub const MAX_FILES: usize = 10;

/// The most bytes of one file.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// The most bytes of all the files of one issue.
pub const MAX_TOTAL_BYTES: u64 = 30 * 1024 * 1024;

/// One download: the bytes and the content type, if Linear sent one.
pub struct Download {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

/// What became of the issue's uploads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fetched {
    /// The folder the files are in, when at least one was saved.
    pub folder: Option<PathBuf>,
    pub saved: usize,
    /// Too many, too large, or the download failed.
    pub left_out: usize,
}

/// Nothing downloaded, for when there is no key: every upload is left out.
pub fn unfetched(d: &Detail) -> Fetched {
    Fetched {
        left_out: links(d).len(),
        ..Fetched::default()
    }
}

/// The upload links in the description and the comments, each once, in order.
pub fn links(d: &Detail) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let texts = std::iter::once(&d.description).chain(d.comments.iter().map(|c| &c.body));
    for text in texts {
        for link in links_in(text) {
            if !out.iter().any(|l| l == link) {
                out.push(link.to_string());
            }
        }
    }
    out
}

fn links_in(text: &str) -> impl Iterator<Item = &str> {
    spans(text).map(move |(start, end)| &text[start..end])
}

/// Where each whole link starts and ends in `text`.
fn spans(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    text.match_indices(HOST).map(move |(i, _)| {
        let rest = &text[i..];
        // Markdown, HTML and plain text end a link with one of these.
        let end = rest
            .find(|c: char| c.is_whitespace() || c.is_control() || "()[]<>\"'`".contains(c))
            .unwrap_or(rest.len());
        (i, i + end)
    })
}

/// `text` with each whole link in `swaps` replaced by its path. A link that
/// only starts with a swapped one (`…/1` and `…/10`) is left alone.
fn swap_links(text: &str, swaps: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end) in spans(text) {
        let link = &text[start..end];
        if let Some((_, path)) = swaps.iter().find(|(l, _)| l == link) {
            out.push_str(&text[last..start]);
            out.push_str(path);
            last = end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Downloads the issue's uploads into `<env>/.arakun-linear` with `get`, and
/// puts each saved file's path in `d` in place of its link. `get` takes the
/// link and the most bytes to read, and fails on a larger file.
pub fn fetch(
    mut get: impl FnMut(&str, u64) -> Result<Download, String>,
    env: &Path,
    d: &mut Detail,
) -> Fetched {
    let links = links(d);
    if links.is_empty() {
        return Fetched::default();
    }
    let folder = env.join(FOLDER);
    if let Err(e) = make_folder(&folder) {
        eprintln!("arakun: could not make the folder for the issue's files: {e}");
        return Fetched {
            left_out: links.len(),
            ..Fetched::default()
        };
    }
    let mut fetched = Fetched::default();
    let mut total = 0u64;
    let mut swaps = Vec::new();
    for link in &links {
        let room = MAX_FILE_BYTES.min(MAX_TOTAL_BYTES - total);
        if fetched.saved == MAX_FILES || room == 0 {
            fetched.left_out += 1;
            continue;
        }
        let saved = get(link, room).and_then(|dl| {
            let name = format!("{}.{}", fetched.saved + 1, extension(&dl, link));
            let path = folder.join(name);
            write_private(&path, &dl.bytes).map_err(|e| e.to_string())?;
            Ok((path, dl.bytes.len() as u64))
        });
        match saved {
            Ok((path, len)) => {
                total += len;
                fetched.saved += 1;
                swaps.push((link.clone(), path.display().to_string()));
            }
            Err(_) => fetched.left_out += 1,
        }
    }
    d.description = swap_links(&d.description, &swaps);
    for c in &mut d.comments {
        c.body = swap_links(&c.body, &swaps);
    }
    if fetched.saved > 0 {
        fetched.folder = Some(folder);
    }
    fetched
}

/// A fresh folder only the person can read, that git ignores: a `.gitignore`
/// of `*` ignores the folder and itself, so nothing is committed by mistake.
fn make_folder(folder: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(folder) {
        // Files of an earlier run on this issue: they may be out of date.
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(folder)?,
        // A link could point anywhere; leave it alone.
        Ok(_) => return Err(std::io::Error::other("something else has its name")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    builder.create(folder)?;
    write_private(&folder.join(".gitignore"), b"*\n")
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(path)?, bytes)
}

/// The file's extension, from its content type or else from its link. Only
/// a short, plain one is kept, so the name stays a safe file name.
fn extension(dl: &Download, link: &str) -> String {
    let from_type = dl
        .content_type
        .as_deref()
        .and_then(|t| t.split(';').next())
        .map(|t| t.trim().to_ascii_lowercase())
        .and_then(|t| {
            Some(match t.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                "image/svg+xml" => "svg",
                "application/pdf" => "pdf",
                "video/mp4" => "mp4",
                "video/quicktime" => "mov",
                "text/plain" => "txt",
                _ => return None,
            })
        });
    if let Some(ext) = from_type {
        return ext.to_string();
    }
    let path = link.split(['?', '#']).next().unwrap_or_default();
    let last = path.rsplit('/').next().unwrap_or_default();
    match last.rsplit_once('.') {
        Some((_, ext))
            if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            ext.to_ascii_lowercase()
        }
        _ => "bin".to_string(),
    }
}

/// Downloads `link` with the person's key, reading at most `limit` bytes.
/// Only a link to Linear's uploads gets the key. Any redirect drops it.
pub fn download(key: &str, link: &str, limit: u64) -> Result<Download, String> {
    if !link.starts_with(HOST) {
        return Err("not a Linear upload".into());
    }
    let request = ureq::http::Request::builder()
        .method("GET")
        .uri(link)
        .header("Authorization", key)
        .body(())
        .map_err(|e| e.to_string())?;
    let mut reply = super::http().run(request).map_err(|e| e.to_string())?;
    let status = reply.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("Linear answered {status}."));
    }
    let content_type = reply
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let bytes = reply
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok(Download {
        bytes,
        content_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::issues::{Comment, Group, Issue};

    fn detail(description: &str, comments: &[&str]) -> Detail {
        Detail {
            issue: Issue {
                identifier: "ENG-1".into(),
                title: "T".into(),
                url: String::new(),
                id: "id".into(),
                priority: 0,
                state: "Todo".into(),
                group: Group::Todo,
                team: "Eng".into(),
                team_id: "t".into(),
                project: None,
                project_id: None,
                branch: String::new(),
                updated_at: 0,
            },
            description: description.into(),
            comments: comments
                .iter()
                .map(|b| Comment {
                    author: "A".into(),
                    body: b.to_string(),
                    ..Comment::default()
                })
                .collect(),
            more_comments: false,
            mention: None,
            labels: Vec::new(),
            parent: None,
            children: Vec::new(),
            relations: Vec::new(),
            links: Vec::new(),
        }
    }

    /// A folder of its own for one test, removed after it.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("arakun-uploads-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn png(_: &str, _: u64) -> Result<Download, String> {
        Ok(Download {
            bytes: b"png".to_vec(),
            content_type: Some("image/png".into()),
        })
    }

    const A: &str = "https://uploads.linear.app/w/a/b";
    const B: &str = "https://uploads.linear.app/w/c/d.jpeg?sig=1";

    #[test]
    fn links_come_from_markdown_html_and_text_once_each() {
        let d = detail(
            &format!("![shot]({A}) and <img src=\"{B}\"> and https://example.com/x"),
            &[&format!("again {A}\nsee"), "https://uploads.linear.appx/no"],
        );
        assert_eq!(links(&d), vec![A.to_string(), B.to_string()]);
    }

    #[test]
    fn each_link_becomes_the_files_path() {
        let env = Tmp::new();
        let mut d = detail(&format!("![shot]({A})"), &[&format!("also {B}")]);
        let f = fetch(png, env.path(), &mut d);
        let folder = env.path().join(FOLDER);
        assert_eq!(f.saved, 2);
        assert_eq!(f.left_out, 0);
        assert_eq!(f.folder.as_deref(), Some(folder.as_path()));
        let one = folder.join("1.png");
        assert_eq!(d.description, format!("![shot]({})", one.display()));
        assert!(d.comments[0].body.ends_with("2.png"));
        assert_eq!(std::fs::read(&one).unwrap(), b"png");
        assert_eq!(
            std::fs::read_to_string(folder.join(".gitignore")).unwrap(),
            "*\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_folder_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let env = Tmp::new();
        let mut d = detail(A, &[]);
        fetch(png, env.path(), &mut d);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let folder = env.path().join(FOLDER);
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&folder.join("1.png")), 0o600);
    }

    #[test]
    fn past_the_caps_files_are_left_out_and_counted() {
        let env = Tmp::new();
        let text = (0..MAX_FILES + 2)
            .map(|i| format!("{HOST}f/{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut d = detail(&text, &[]);
        let f = fetch(png, env.path(), &mut d);
        assert_eq!((f.saved, f.left_out), (MAX_FILES, 2));
        // The ones left out keep their link.
        assert!(
            d.description
                .ends_with(&format!("{HOST}f/{}", MAX_FILES + 1))
        );
    }

    #[test]
    fn the_total_cap_holds_and_a_failure_is_left_out() {
        let env = Tmp::new();
        let mut d = detail(&format!("{HOST}1 {HOST}2 {HOST}3 {HOST}4 {HOST}5"), &[]);
        let mut asked = Vec::new();
        let f = fetch(
            |link, limit| {
                asked.push(limit);
                if link.ends_with('2') {
                    return Err("refused".into());
                }
                Ok(Download {
                    bytes: vec![0; limit as usize],
                    content_type: None,
                })
            },
            env.path(),
            &mut d,
        );
        // 10 MB, failed, 10 MB, 10 MB: the total is used up.
        assert_eq!((f.saved, f.left_out), (3, 2));
        assert_eq!(asked.len(), 4);
        assert!(asked.iter().all(|&l| l == MAX_FILE_BYTES));
    }

    #[test]
    fn an_earlier_runs_files_are_cleared() {
        let env = Tmp::new();
        let folder = env.path().join(FOLDER);
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("1.png"), b"old").unwrap();
        let mut d = detail(A, &[]);
        assert_eq!(fetch(png, env.path(), &mut d).saved, 1);
        assert_eq!(std::fs::read(folder.join("1.png")).unwrap(), b"png");
    }

    #[test]
    fn no_links_touch_nothing() {
        let env = Tmp::new();
        let mut d = detail("plain", &[]);
        assert_eq!(fetch(png, env.path(), &mut d), Fetched::default());
        assert!(!env.path().join(FOLDER).exists());
    }

    #[test]
    fn the_extension_is_short_and_plain() {
        let dl = |t: Option<&str>| Download {
            bytes: Vec::new(),
            content_type: t.map(str::to_string),
        };
        assert_eq!(extension(&dl(Some("image/jpeg; x=1")), A), "jpg");
        assert_eq!(extension(&dl(None), B), "jpeg");
        assert_eq!(extension(&dl(None), A), "bin");
        assert_eq!(extension(&dl(None), &format!("{HOST}a/b.p-g")), "bin");
        assert_eq!(extension(&dl(None), &format!("{HOST}a/b.toolong")), "bin");
    }

    #[test]
    fn the_key_goes_only_to_linears_uploads() {
        assert!(download("k", "https://example.com/x", 10).is_err());
    }
}
