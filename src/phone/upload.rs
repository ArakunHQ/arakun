//! Photos a phone sends with a prompt. A relay frame is small, so a photo
//! comes in pieces, each base64, under an id the phone makes up. Once the last
//! piece lands the photo is saved, and the prompt that names the id carries
//! the file as an `@` mention, the way a pasted image does on the desktop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine as _;

/// How long half-sent photos are kept before the pieces are dropped.
const LIVES: Duration = Duration::from_secs(30 * 60);
/// The most pieces one photo may come in.
const MAX_PARTS: usize = 512;

struct Upload {
    parts: Vec<Option<Vec<u8>>>,
    name: String,
    at: Instant,
    /// Where it landed once its last piece arrived.
    path: Option<PathBuf>,
}

#[derive(Default)]
pub struct Uploads {
    by_id: HashMap<String, Upload>,
}

impl Uploads {
    /// Takes piece `part` of `of` of the photo `upload`. Once every piece is
    /// here, `save` gets the bytes and the extension to save them under, and
    /// the file it made is returned (again for a piece that comes twice).
    pub fn piece(
        &mut self,
        upload: &str,
        name: &str,
        part: usize,
        of: usize,
        data: &str,
        save: impl FnOnce(&[u8], &str) -> std::io::Result<PathBuf>,
    ) -> Option<PathBuf> {
        self.by_id.retain(|_, u| u.at.elapsed() < LIVES);
        let entry = self
            .by_id
            .entry(upload.to_string())
            .or_insert_with(|| Upload {
                parts: vec![None; of.clamp(1, MAX_PARTS)],
                name: name.to_string(),
                at: Instant::now(),
                path: None,
            });
        if let (Some(slot), Ok(bytes)) = (
            entry.parts.get_mut(part),
            base64::engine::general_purpose::STANDARD.decode(data.as_bytes()),
        ) {
            *slot = (!bytes.is_empty()).then_some(bytes);
        }
        if entry.path.is_none() && entry.parts.iter().all(|p| p.is_some()) {
            let bytes: Vec<u8> = entry.parts.iter().flatten().flatten().cloned().collect();
            if bytes.len() <= crate::agent::attach::MAX_IMAGE_BYTES as usize
                && let Ok(path) = save(&bytes, ext(&entry.name))
            {
                entry.path = Some(path);
            }
        }
        entry.path.clone()
    }

    /// `text` with the saved photos of `ids` after it as `@` mentions. The ids
    /// are spent; one that never finished adds nothing.
    pub fn attach(&mut self, text: String, ids: &[String]) -> String {
        let paths: Vec<String> = ids
            .iter()
            .filter_map(|id| self.by_id.remove(id).and_then(|u| u.path))
            .map(|path| format!("@{}", path.display()))
            .collect();
        if paths.is_empty() {
            text
        } else {
            format!("{} {}", text.trim_end(), paths.join(" "))
        }
    }
}

/// The extension a phone's photo is saved under: what its name says when it
/// says something short and plain, jpg otherwise (the page re-encodes to JPEG).
fn ext(name: &str) -> &str {
    let ext = name.rsplit('.').next().unwrap_or("");
    if matches!(ext, "png" | "jpg" | "jpeg" | "webp" | "gif") {
        ext
    } else {
        "jpg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn a_photo_is_saved_once_its_last_piece_lands() {
        let mut uploads = Uploads::default();
        let saved = std::cell::RefCell::new(Vec::new());
        let save = |bytes: &[u8], ext: &str| {
            saved.borrow_mut().push((bytes.to_vec(), ext.to_string()));
            Ok(PathBuf::from("/tmp/photo.png"))
        };
        // Out of order: nothing is saved until both are here.
        assert_eq!(uploads.piece("u1", "a.png", 1, 2, &b64(b"cd"), save), None);
        assert_eq!(
            uploads.piece("u1", "a.png", 0, 2, &b64(b"ab"), save),
            Some(PathBuf::from("/tmp/photo.png"))
        );
        assert_eq!(*saved.borrow(), [(b"abcd".to_vec(), "png".to_string())]);
        // A piece that comes again finds the file and saves nothing new.
        assert!(
            uploads
                .piece("u1", "a.png", 0, 2, &b64(b"ab"), save)
                .is_some()
        );
        assert_eq!(saved.borrow().len(), 1);

        let text = uploads.attach("look  ".into(), &["u1".into(), "nope".into()]);
        assert_eq!(text, "look @/tmp/photo.png");
        // Spent with the prompt that carried it.
        assert_eq!(uploads.attach("again".into(), &["u1".into()]), "again");
    }

    #[test]
    fn a_piece_out_of_range_or_not_base64_is_left_out() {
        let mut uploads = Uploads::default();
        let never = |_: &[u8], _: &str| -> std::io::Result<PathBuf> { panic!("not complete") };
        assert_eq!(uploads.piece("u", "a.jpg", 5, 2, &b64(b"x"), never), None);
        assert_eq!(uploads.piece("u", "a.jpg", 0, 2, "***", never), None);
    }

    #[test]
    fn odd_names_are_saved_as_jpeg() {
        assert_eq!(ext("shot.webp"), "webp");
        assert_eq!(ext("IMG_0001.HEIC"), "jpg");
        assert_eq!(ext("no extension"), "jpg");
    }
}
