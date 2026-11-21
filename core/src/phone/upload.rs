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

/// How much a device may leave here. Pieces are kept in memory until the
/// photo is whole, and a paired device can start as many photos as it likes
/// and finish none: without these, that fills the memory of the one process
/// every session runs in.
#[derive(Clone, Copy)]
struct Limits {
    /// The most bytes one photo may hold: no photo larger than this is saved.
    one: usize,
    /// The most bytes all half-sent photos may hold together.
    all: usize,
    /// The most photos kept at once, half-sent or waiting for their prompt.
    count: usize,
}

const LIMITS: Limits = Limits {
    one: crate::agent::attach::MAX_IMAGE_BYTES as usize,
    all: 3 * crate::agent::attach::MAX_IMAGE_BYTES as usize,
    count: 32,
};

struct Upload {
    parts: Vec<Option<Vec<u8>>>,
    /// The bytes its pieces hold so far.
    held: usize,
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
    /// A photo that grows past what one may hold, or past what all may hold
    /// together, is dropped with the pieces it had.
    pub fn piece(
        &mut self,
        upload: &str,
        name: &str,
        part: usize,
        of: usize,
        data: &str,
        save: impl FnOnce(&[u8], &str) -> std::io::Result<PathBuf>,
    ) -> Option<PathBuf> {
        self.piece_within(LIMITS, upload, name, part, of, data, save)
    }

    #[allow(clippy::too_many_arguments)] // `piece`, with the limits a test can make small.
    fn piece_within(
        &mut self,
        limits: Limits,
        upload: &str,
        name: &str,
        part: usize,
        of: usize,
        data: &str,
        save: impl FnOnce(&[u8], &str) -> std::io::Result<PathBuf>,
    ) -> Option<PathBuf> {
        self.by_id.retain(|_, u| u.at.elapsed() < LIVES);
        if !self.by_id.contains_key(upload) && self.by_id.len() >= limits.count {
            return None;
        }
        let others: usize = self
            .by_id
            .iter()
            .filter(|(id, _)| id.as_str() != upload)
            .map(|(_, u)| u.held)
            .sum();
        let entry = self
            .by_id
            .entry(upload.to_string())
            .or_insert_with(|| Upload {
                parts: vec![None; of.clamp(1, MAX_PARTS)],
                held: 0,
                name: name.to_string(),
                at: Instant::now(),
                path: None,
            });
        if let (Some(slot), Ok(bytes)) = (
            entry.parts.get_mut(part),
            base64::engine::general_purpose::STANDARD.decode(data.as_bytes()),
        ) {
            // A piece sent again takes the place of the one before it.
            let held = entry.held - slot.as_ref().map_or(0, Vec::len) + bytes.len();
            if held > limits.one || others + held > limits.all {
                self.by_id.remove(upload);
                return None;
            }
            entry.held = held;
            *slot = (!bytes.is_empty()).then_some(bytes);
        }
        if entry.path.is_none() && entry.parts.iter().all(|p| p.is_some()) {
            let bytes: Vec<u8> = entry.parts.iter().flatten().flatten().cloned().collect();
            if let Ok(path) = save(&bytes, ext(&entry.name)) {
                entry.path = Some(path);
                // On disk now: only where it is has to be remembered.
                (entry.parts, entry.held) = (Vec::new(), 0);
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
            // Quoted when the path has a space in it, or the prompt would name half of it.
            .map(|path| crate::agent::attach::mention(&path.to_string_lossy()))
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
    fn a_photo_in_a_folder_with_a_space_is_still_read_as_one_path() {
        let mut uploads = Uploads::default();
        let save = |_: &[u8], _: &str| Ok(PathBuf::from("/srv/Agent Data/uploads/photo-1.jpg"));
        assert!(
            uploads
                .piece("u", "a.jpg", 0, 1, &b64(b"x"), save)
                .is_some()
        );
        let text = uploads.attach("look".into(), &["u".into()]);
        assert_eq!(text, "look @\"/srv/Agent Data/uploads/photo-1.jpg\"");
        assert_eq!(
            crate::agent::attach::mentions(&text),
            ["/srv/Agent Data/uploads/photo-1.jpg"]
        );
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
    #[test]
    fn a_device_cannot_leave_more_than_the_limits_in_memory() {
        let limits = Limits {
            one: 8,
            all: 12,
            count: 3,
        };
        let mut uploads = Uploads::default();
        let held = |u: &Uploads| u.by_id.values().map(|u| u.held).sum::<usize>();
        let save = |_: &[u8], _: &str| Ok(PathBuf::from("/tmp/photo.jpg"));
        let piece = |u: &mut Uploads, id: &str, part: usize, bytes: &[u8]| {
            u.piece_within(limits, id, "a.jpg", part, 3, &b64(bytes), save)
        };

        // The first piece of a photo that is never finished, again and again.
        assert_eq!(piece(&mut uploads, "a", 0, b"1234"), None);
        assert_eq!(piece(&mut uploads, "b", 0, b"1234"), None);
        assert_eq!(piece(&mut uploads, "c", 0, b"1234"), None);
        assert_eq!((uploads.by_id.len(), held(&uploads)), (3, 12));
        // A fourth photo is not taken in while three are kept.
        assert_eq!(piece(&mut uploads, "d", 0, b"1"), None);
        assert!(!uploads.by_id.contains_key("d"));
        // One more byte than all may hold together: that photo goes, pieces and all.
        assert_eq!(piece(&mut uploads, "a", 1, b"5"), None);
        assert_eq!((uploads.by_id.len(), held(&uploads)), (2, 8));
        // A photo that grows past what one may hold goes too.
        uploads.by_id.remove("c");
        assert_eq!(piece(&mut uploads, "b", 1, b"5678"), None);
        assert_eq!(held(&uploads), 8);
        assert_eq!(piece(&mut uploads, "b", 2, b"9"), None);
        assert!(uploads.by_id.is_empty());
        // The same piece again replaces itself and is counted once.
        assert_eq!(piece(&mut uploads, "e", 0, b"1234"), None);
        assert_eq!(piece(&mut uploads, "e", 0, b"1234"), None);
        assert_eq!(held(&uploads), 4);
        // Whole and saved, a photo holds nothing here but where it is.
        assert_eq!(piece(&mut uploads, "e", 1, b"56"), None);
        assert!(piece(&mut uploads, "e", 2, b"78").is_some());
        assert_eq!(held(&uploads), 0);
        assert!(piece(&mut uploads, "e", 0, b"1234").is_some());
    }
}
