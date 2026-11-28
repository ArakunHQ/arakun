//! Files a prompt to a server carries. A prompt names an attached file by
//! its path on this computer, which the server cannot read. So each one goes
//! ahead of the prompt as `upload` pieces, as a phone sends its photos, and
//! the prompt names it by the upload's id instead. The server saves it and
//! puts its own path back in the prompt.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::agent::attach;
use crate::phone::{MAX_FILE, MAX_PROMPT};

/// The base64 one piece carries at most: under the relay's frame once sealed,
/// as the phone page's pieces are.
pub const PIECE: usize = 160 * 1024;

/// How many sent files are remembered, to put a prompt that did not go back
/// in the composer as it was written.
const REMEMBERED: usize = 64;

/// A file on this computer that a prompt mentions.
#[derive(Debug, PartialEq)]
pub struct Upload {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// An image, which every server takes. Any other file only goes to a
    /// server that takes files.
    pub image: bool,
}

/// What [`local_files`] made of a prompt.
#[derive(Debug, Default, PartialEq)]
pub struct Local {
    /// The prompt without the mentions of the files that go.
    pub text: String,
    pub files: Vec<Upload>,
    /// Files on this computer too large to send, alone or with the others:
    /// they stay mentions, which the server cannot read.
    pub too_large: Vec<PathBuf>,
}

/// `text` without its mentions of files on this computer, and those files
/// read: any regular file when the server takes `files`, its images only
/// otherwise. Only a full path (or one under `~/`) can be a file here: a
/// relative one names a file in the server's folder.
pub fn local_files(text: &str, files: bool) -> Local {
    let mut too_large = Vec::new();
    let mut total = 0;
    let (text, files) = attach::split_mentions(text, |m| {
        if !(Path::new(m).is_absolute() || m.starts_with("~/")) {
            return None;
        }
        let path = attach::mentioned_file(m, Path::new("/"))?;
        let image = attach::mime_type(&path).is_some();
        if !(image || files) {
            return None;
        }
        // The server drops a larger one, so it stays a mention.
        let len = std::fs::metadata(&path).ok()?.len();
        if len > MAX_FILE || total + len > MAX_PROMPT {
            too_large.push(path);
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        total += len;
        Some(Upload { path, bytes, image })
    });
    Local {
        text,
        files,
        too_large,
    }
}

/// The notice for files left out of a prompt for their size.
pub fn too_large_notice(paths: &[PathBuf]) -> Option<String> {
    let names: Vec<String> = paths
        .iter()
        .map(|p| {
            p.file_name()
                .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
                .into_owned()
        })
        .collect();
    let mb = |b: u64| b / (1024 * 1024);
    let limits = format!(
        "A server takes files up to {} MB each and {} MB for one prompt",
        mb(MAX_FILE),
        mb(MAX_PROMPT)
    );
    match names.as_slice() {
        [] => None,
        [one] => Some(format!(
            "{one} is too large to send, so the agent cannot open it. {limits}."
        )),
        _ => Some(format!(
            "{} are too large to send, so the agent cannot open them. {limits}.",
            names.join(", ")
        )),
    }
}

/// The `upload` messages that carry `file` to the server as upload `id`.
pub fn pieces(session: &str, id: &str, file: &Upload) -> Vec<Value> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&file.bytes);
    let name = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Base64 is ASCII, so any byte is a place to cut it.
    let chunks: Vec<&str> = if b64.is_empty() {
        vec![""]
    } else {
        b64.as_bytes()
            .chunks(PIECE)
            .map(|c| std::str::from_utf8(c).unwrap_or_default())
            .collect()
    };
    let of = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(part, data)| {
            let mut piece = json!({
                "type": "upload", "session": session, "upload": id, "name": name,
                "part": part, "of": of, "data": data,
            });
            // Left out for an image: a server from before files saves it the same.
            if !file.image {
                piece["file"] = true.into();
            }
            piece
        })
        .collect()
}

/// The files sent lately, by upload id, so a prompt that did not reach the
/// server goes back in the composer with them.
#[derive(Default)]
pub struct Sent {
    ids: VecDeque<(String, PathBuf)>,
    made: u64,
}

impl Sent {
    /// A new upload id for `path`, remembered with it.
    pub fn id(&mut self, path: PathBuf) -> String {
        self.made += 1;
        let id = format!("app-{}-{}", crate::model::unix_now(), self.made);
        if self.ids.len() >= REMEMBERED {
            self.ids.pop_front();
        }
        self.ids.push_back((id.clone(), path));
        id
    }

    /// `text` with the files of `ids` after it as mentions again.
    pub fn put_back(&mut self, text: &str, ids: &[String]) -> String {
        let mentions: Vec<String> = ids
            .iter()
            .filter_map(|id| {
                let at = self.ids.iter().position(|(i, _)| i == id)?;
                let (_, path) = self.ids.remove(at)?;
                Some(attach::mention(&path.to_string_lossy()))
            })
            .collect();
        match (text.trim_end(), mentions.is_empty()) {
            (text, true) => text.to_string(),
            ("", false) => mentions.join(" "),
            (text, false) => format!("{text} {}", mentions.join(" ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-upload {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn takes_out_the_files_on_this_computer_only() {
        let dir = dir().join("only");
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("shot one.png");
        let log = dir.join("server.log");
        std::fs::write(&shot, b"png").unwrap();
        std::fs::write(&log, b"boot").unwrap();
        let text = format!(
            "look {} and {} and @src/logo.png and @/nowhere/x.png and @{}",
            attach::mention(&shot.to_string_lossy()),
            attach::mention(&log.to_string_lossy()),
            dir.display(),
        );
        let local = local_files(&text, true);
        assert_eq!(
            local.text,
            format!(
                "look  and  and @src/logo.png and @/nowhere/x.png and @{}",
                dir.display()
            )
        );
        let shot = Upload {
            path: shot,
            bytes: b"png".to_vec(),
            image: true,
        };
        let log = Upload {
            path: log,
            bytes: b"boot".to_vec(),
            image: false,
        };
        assert_eq!(local.files, [shot, log]);

        // A server from before files gets the images only.
        let older = local_files(&text, false);
        assert_eq!(older.files.len(), 1);
        assert!(older.files[0].image);
        assert!(older.text.contains("server.log"));
    }

    #[test]
    fn a_file_too_large_stays_and_is_named() {
        let dir = dir().join("large");
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.csv");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_FILE + 1)
            .unwrap();
        // Three that fit one by one, but not together.
        let parts: Vec<PathBuf> = (0..4)
            .map(|n| {
                let p = dir.join(format!("part{n}.bin"));
                std::fs::File::create(&p)
                    .unwrap()
                    .set_len(MAX_FILE)
                    .unwrap();
                p
            })
            .collect();
        let text = std::iter::once(&big)
            .chain(&parts)
            .map(|p| attach::mention(&p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        let local = local_files(&text, true);
        assert_eq!(local.files.len(), 3);
        assert_eq!(local.too_large, [big.clone(), parts[3].clone()]);
        assert!(local.text.contains("big.csv") && local.text.contains("part3.bin"));
        assert_eq!(
            too_large_notice(&local.too_large).unwrap(),
            "big.csv, part3.bin are too large to send, so the agent cannot open them. \
             A server takes files up to 5 MB each and 15 MB for one prompt."
        );
        assert_eq!(too_large_notice(&[]), None);
    }

    #[test]
    fn only_a_file_that_is_not_an_image_is_marked_as_one() {
        let log = Upload {
            path: PathBuf::from("/p/server.log"),
            bytes: b"boot".to_vec(),
            image: false,
        };
        assert_eq!(pieces("s1", "u1", &log)[0]["file"], true);
        let shot = Upload { image: true, ..log };
        assert!(pieces("s1", "u1", &shot)[0].get("file").is_none());
    }

    #[test]
    fn cuts_an_image_into_pieces_that_join_back() {
        let image = Upload {
            path: PathBuf::from("/p/a.png"),
            bytes: (0..=255u8).cycle().take(PIECE).collect(),
            image: true,
        };
        let pieces = pieces("s1", "u1", &image);
        // PIECE bytes make 4/3 as much base64: two pieces.
        assert_eq!(pieces.len(), 2);
        let data: String = pieces
            .iter()
            .map(|p| {
                assert_eq!(
                    (&p["session"], &p["upload"], &p["name"]),
                    (&json!("s1"), &json!("u1"), &json!("a.png"))
                );
                assert_eq!(p["of"], 2);
                p["data"].as_str().unwrap().to_string()
            })
            .collect();
        assert!(
            pieces
                .iter()
                .all(|p| p["data"].as_str().unwrap().len() <= PIECE)
        );
        let back = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(back, image.bytes);
    }

    #[test]
    fn the_server_saves_what_the_pieces_carry() {
        let image = Upload {
            path: PathBuf::from("/p/shot.png"),
            bytes: (0..=255u8).cycle().take(3 * PIECE).collect(),
            image: true,
        };
        let mut uploads = crate::phone::Uploads::default();
        let mut saved = None;
        for p in pieces("s1", "u1", &image) {
            let (part, of) = (p["part"].as_u64().unwrap(), p["of"].as_u64().unwrap());
            let data = p["data"].as_str().unwrap();
            let name = p["name"].as_str().unwrap();
            saved = uploads.piece(
                "u1",
                name,
                false,
                part as usize,
                of as usize,
                data,
                |bytes, what| {
                    assert_eq!(
                        (bytes, what),
                        (image.bytes.as_slice(), crate::phone::Save::Image("png"))
                    );
                    Ok(PathBuf::from("/srv/uploads/shot.png"))
                },
            );
        }
        assert_eq!(saved, Some(PathBuf::from("/srv/uploads/shot.png")));
        assert_eq!(
            uploads.attach("look".into(), &["u1".into()]),
            "look @/srv/uploads/shot.png"
        );
    }

    #[test]
    fn puts_the_images_of_a_lost_prompt_back() {
        let mut sent = Sent::default();
        let a = sent.id(PathBuf::from("/p/a b.png"));
        let b = sent.id(PathBuf::from("/p/c.png"));
        assert_ne!(a, b);
        assert_eq!(
            sent.put_back("look", &[a.clone(), b.clone()]),
            "look @\"/p/a b.png\" @/p/c.png"
        );
        // Spent once put back.
        assert_eq!(sent.put_back("look", &[a]), "look");
        let d = sent.id(PathBuf::from("/p/d.png"));
        assert_eq!(sent.put_back("", &[d]), "@/p/d.png");
    }
}
