//! Images a prompt to a server carries. A prompt names a pasted image by its
//! path on this computer, which the server cannot read. So each one goes
//! ahead of the prompt as `upload` pieces, as a phone sends its photos, and
//! the prompt names it by the upload's id instead. The server saves it and
//! puts its own path back in the prompt.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::agent::attach;

/// The base64 one piece carries at most: under the relay's frame once sealed,
/// as the phone page's pieces are.
pub const PIECE: usize = 160 * 1024;

/// How many sent images are remembered, to put a prompt that did not go back
/// in the composer as it was written.
const REMEMBERED: usize = 64;

/// An image file on this computer that a prompt mentions.
#[derive(Debug, PartialEq)]
pub struct Image {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// `text` without its mentions of image files on this computer, and those
/// images read. Only a full path (or one under `~/`) can be a file here: a
/// relative one names a file in the server's folder.
pub fn local_images(text: &str) -> (String, Vec<Image>) {
    attach::split_mentions(text, |m| {
        if !(Path::new(m).is_absolute() || m.starts_with("~/")) {
            return None;
        }
        let path = attach::mentioned_image(m, Path::new("/"))?;
        // The server drops a larger one, so it stays a mention.
        if std::fs::metadata(&path).ok()?.len() > attach::MAX_IMAGE_BYTES {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        Some(Image { path, bytes })
    })
}

/// The `upload` messages that carry `image` to the server as upload `id`.
pub fn pieces(session: &str, id: &str, image: &Image) -> Vec<Value> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
    let name = image
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
            json!({
                "type": "upload", "session": session, "upload": id, "name": name,
                "part": part, "of": of, "data": data,
            })
        })
        .collect()
}

/// The images sent lately, by upload id, so a prompt that did not reach the
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

    /// `text` with the images of `ids` after it as mentions again.
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
    fn takes_out_the_images_on_this_computer_only() {
        let dir = dir();
        let shot = dir.join("shot one.png");
        std::fs::write(&shot, b"png").unwrap();
        let text = format!(
            "look {} and @src/logo.png and @/nowhere/x.png",
            attach::mention(&shot.to_string_lossy())
        );
        let (text, images) = local_images(&text);
        assert_eq!(text, "look  and @src/logo.png and @/nowhere/x.png");
        assert_eq!(
            images,
            [Image {
                path: shot,
                bytes: b"png".to_vec()
            }]
        );
    }

    #[test]
    fn cuts_an_image_into_pieces_that_join_back() {
        let image = Image {
            path: PathBuf::from("/p/a.png"),
            bytes: (0..=255u8).cycle().take(PIECE).collect(),
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
        let image = Image {
            path: PathBuf::from("/p/shot.png"),
            bytes: (0..=255u8).cycle().take(3 * PIECE).collect(),
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
                part as usize,
                of as usize,
                data,
                |bytes, ext| {
                    assert_eq!((bytes, ext), (image.bytes.as_slice(), "png"));
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
