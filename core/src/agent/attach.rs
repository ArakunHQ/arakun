//! Images and PDFs attached to a prompt with `@path` mentions (`@"path with
//! spaces"` quotes a path). The text is sent as written; the images go
//! alongside it, as paths for Droid and as base64 blocks for ACP agents, and
//! PDFs as document blocks to the agents that take them.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};

/// The Anthropic API rejects larger images, so they are not worth sending.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// The Anthropic API takes requests up to 32 MB, and base64 makes a file a
/// third bigger, so a larger PDF would not fit.
pub const MAX_PDF_BYTES: u64 = 24 * 1024 * 1024;

/// A PDF, by its extension.
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// A file that goes along with a prompt instead of as a mention alone: an
/// image, or a PDF.
pub fn is_attachable(path: &Path) -> bool {
    (mime_type(path).is_some() || is_pdf(path)) && path.is_file()
}

pub fn mime_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

/// The paths written after `@` in `text`, in order and without repeats.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let starts_token = at == 0 || rest[..at].ends_with(char::is_whitespace);
        let after = &rest[at + 1..];
        let (path, next) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], &quoted[end + 1..]),
                None => ("", after),
            }
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        let path = path.trim_end_matches([',', ';', ')']);
        if starts_token && !path.is_empty() && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
        rest = next;
    }
    out
}

/// How a path is written in a prompt so that [`mentions`] reads it back.
pub fn mention(path: &str) -> String {
    if path.contains(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    }
}

fn resolve(mention: &str, cwd: &Path) -> Option<PathBuf> {
    Some(match mention.strip_prefix("~/") {
        Some(rest) => crate::platform::home().join(rest),
        None => cwd.join(mention),
    })
}

fn is_image_file(path: &Path) -> bool {
    mime_type(path).is_some() && path.is_file()
}

/// The image files `text` mentions, resolved against `cwd`.
pub fn images(text: &str, cwd: &Path) -> Vec<PathBuf> {
    mentions(text)
        .into_iter()
        .filter_map(|m| resolve(&m, cwd))
        .filter(|p| is_image_file(p))
        .collect()
}

/// A prompt without its image mentions, and the images, to show them as
/// pictures instead of paths.
pub fn split_images(text: &str, cwd: &Path) -> (String, Vec<PathBuf>) {
    split_mentions(text, |m| mentioned_image(m, cwd))
}

/// The PDF files `text` mentions, resolved against `cwd`.
pub fn documents(text: &str, cwd: &Path) -> Vec<PathBuf> {
    mentions(text)
        .into_iter()
        .filter_map(|m| resolve(&m, cwd))
        .filter(|p| is_pdf(p) && p.is_file())
        .collect()
}

/// A prompt without its PDF mentions, and the PDFs, to show them as chips
/// instead of paths.
pub fn split_documents(text: &str, cwd: &Path) -> (String, Vec<PathBuf>) {
    split_mentions(text, |m| {
        resolve(m, cwd).filter(|p| is_pdf(p) && p.is_file())
    })
}

/// The regular file a mention written as `mention` names, when it is one.
pub fn mentioned_file(mention: &str, cwd: &Path) -> Option<PathBuf> {
    resolve(mention, cwd).filter(|p| p.is_file())
}

/// The image file a mention written as `mention` names, when it is one.
pub fn mentioned_image(mention: &str, cwd: &Path) -> Option<PathBuf> {
    resolve(mention, cwd).filter(|p| is_image_file(p))
}

/// The mentions in `text` that name images by their extension, as written.
/// For a server's session, whose files are not here to look at.
pub fn image_mentions(text: &str) -> Vec<String> {
    split_mentions(text, |m| mime_type(Path::new(m)).map(|_| m.to_string())).1
}

/// A prompt without the mentions `image` takes, and what it made of them.
/// Split as [`split_images`] does, whatever the images are read from.
pub fn split_mentions<T: PartialEq>(
    text: &str,
    mut image: impl FnMut(&str) -> Option<T>,
) -> (String, Vec<T>) {
    let mut out = String::with_capacity(text.len());
    let mut found: Vec<T> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let starts_token = at == 0 || rest[..at].ends_with(char::is_whitespace);
        let after = &rest[at + 1..];
        let (path, len) = match after.strip_prefix('"') {
            Some(quoted) => match quoted.find('"') {
                Some(end) => (&quoted[..end], end + 2),
                None => ("", 0),
            },
            None => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..end], end)
            }
        };
        let image = (starts_token && !path.is_empty())
            .then(|| image(path))
            .flatten();
        match image {
            Some(p) => {
                out.push_str(&rest[..at]);
                if !found.contains(&p) {
                    found.push(p);
                }
                rest = &after[len..];
            }
            None => {
                out.push_str(&rest[..at + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let text = out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    (text.trim().to_string(), found)
}

/// A prompt written in `from` that starts in `to` instead: a mention of a
/// file that is in `from` but not in `to` (a new image, an untracked file)
/// is written as its full path in `from`, so the agent still finds it.
pub fn move_mentions(text: &str, from: &Path, to: &Path) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let starts_token = at == 0 || rest[..at].ends_with(char::is_whitespace);
        let after = &rest[at + 1..];
        // `len` is how much of `after` the mention takes. Punctuation after
        // an unquoted one is not part of the path, and stays.
        let (path, len) = match after.strip_prefix('"') {
            Some(quoted) => match quoted.find('"') {
                Some(end) => (&quoted[..end], end + 2),
                None => ("", 0),
            },
            None => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                let path = after[..end].trim_end_matches([',', ';', ')']);
                (path, path.len())
            }
        };
        let moved = (starts_token
            && !path.is_empty()
            && !path.starts_with("~/")
            && Path::new(path).is_relative())
        .then(|| from.join(path))
        .filter(|old| old.exists() && !to.join(path).exists());
        match moved {
            Some(old) => {
                out.push_str(&rest[..at]);
                out.push_str(&mention(&old.to_string_lossy()));
                rest = &after[len..];
            }
            None => {
                out.push_str(&rest[..at + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Pastes this big go along as an attachment instead of filling the composer.
pub fn is_large_paste(text: &str) -> bool {
    text.len() > 2000 || text.lines().count() > 40
}

/// How a pasted block travels in a prompt, so [`split_pastes`] can fold it.
pub fn wrap_paste(text: &str) -> String {
    format!(
        "<pasted-text lines=\"{}\">\n{}\n</pasted-text>",
        text.lines().count(),
        text.trim_end()
    )
}

/// A prompt as typed, plus the pasted blocks it carries.
pub fn split_pastes(text: &str) -> (String, Vec<String>) {
    const OPEN: &str = "<pasted-text";
    const CLOSE: &str = "</pasted-text>";
    let (mut typed, mut pastes, mut rest) = (String::new(), Vec::new(), text);
    while let Some(start) = rest.find(OPEN) {
        let Some(body) = rest[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let end = rest[body..].find(CLOSE).map_or(rest.len(), |i| body + i);
        typed.push_str(&rest[..start]);
        pastes.push(rest[body..end].trim_matches('\n').to_string());
        rest = &rest[(end + CLOSE.len()).min(rest.len())..];
    }
    typed.push_str(rest);
    (typed.trim().to_string(), pastes)
}

/// What a session title is made from: the typed words, else the first paste.
fn title_text(text: &str) -> String {
    let (typed, mut pastes) = split_pastes(text);
    if !typed.is_empty() || pastes.is_empty() {
        return typed;
    }
    pastes.swap_remove(0)
}

/// How much of a prompt a title is looked for in; a title is its first line
/// with words, and a long paste need not be read whole for it.
const TITLE_SCAN: usize = 4096;

/// What a session is called when every line of its prompt is a secret.
pub const UNTITLED: &str = "Untitled session";

/// The title a prompt gives its session: its first line with words in it, cut
/// to `max` characters. Titles go to notices, the phone's lock screen and
/// other models, so a secret never reaches one: a secret inside a line is
/// replaced with [`crate::secrets::SECRET`], and a line that is only a secret
/// is skipped. A prompt with nothing but secrets gets [`UNTITLED`].
pub fn title(text: &str, max: usize) -> String {
    let text = title_text(text);
    let mut end = text.len().min(TITLE_SCAN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = without_handles(&crate::secrets::scrub(&text[..end]));
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.clone().next() else {
        return String::new();
    };
    let has_words = |l: &&str| {
        l.replace(crate::secrets::SECRET, "")
            .chars()
            .any(char::is_alphanumeric)
    };
    match lines.find(has_words) {
        Some(line) => crate::agent::log::head(line, max),
        None if first.contains(crate::secrets::SECRET) => UNTITLED.to_string(),
        None => crate::agent::log::head(first, max),
    }
}

/// `text` with each secret handle (`secret-ref:…`) as
/// [`crate::secrets::SECRET`]: a handle saved in place of a pasted secret is
/// not the value, but it is no title either.
fn without_handles(text: &str) -> String {
    let prefix = crate::agent::secret::HANDLE_PREFIX;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        out.push_str(&rest[..at]);
        out.push_str(crate::secrets::SECRET);
        let after = &rest[at + prefix.len()..];
        let len = after
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(after.len());
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}

/// Saves the image in an ACP `image` block a tool returned in `dir`, the
/// folder of pasted images, and gives its path. `None` for an image that
/// is not a PNG, JPEG, GIF or WebP, is too big to send, or cannot be read.
///
/// It is named like a pasted one, `paste-<digits>.<ext>`, so the session that
/// holds it deletes it with its own. The digits come from the session and the
/// bytes: an agent sends a call's content again with each update and when a
/// session loads, and the same image is then written once.
pub fn save_tool_image(dir: &Path, session: &str, block: &Value) -> Option<PathBuf> {
    use sha2::Digest as _;
    let ext = match block["mimeType"].as_str()? {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    };
    let data = block["data"].as_str()?;
    // Base64 is a third bigger; a larger one is not worth decoding.
    if data.len() as u64 > MAX_IMAGE_BYTES / 3 * 4 + 4 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    let mut hash = sha2::Sha256::new();
    hash.update(session.as_bytes());
    hash.update([0]);
    hash.update(&bytes);
    let digest = hash.finalize();
    let n = u64::from_be_bytes(digest[..8].try_into().ok()?);
    let file = dir.join(format!("paste-{n}.{ext}"));
    if !file.is_file() {
        crate::durable::write_private(&file, &bytes).ok()?;
    }
    Some(file)
}

/// An ACP `image` content block, or why the file can't be sent.
pub fn acp_block(path: &Path) -> Result<Value, String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mime = mime_type(path).ok_or_else(|| format!("{name} is not an image"))?;
    let data = read_base64(path, MAX_IMAGE_BYTES)?;
    Ok(json!({
        "type": "image",
        "mimeType": mime,
        "data": data,
        "uri": format!("file://{}", path.display()),
    }))
}

/// A PDF as an ACP embedded resource, or why it can't be sent. Claude's
/// adapter turns a PDF blob into a `document` block for the model.
pub fn acp_document_block(path: &Path) -> Result<Value, String> {
    if !is_pdf(path) {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        return Err(format!("{name} is not a PDF"));
    }
    let data = read_base64(path, MAX_PDF_BYTES)?;
    Ok(json!({
        "type": "resource",
        "resource": {
            "uri": format!("file://{}", path.display()),
            "mimeType": "application/pdf",
            "blob": data,
        },
    }))
}

/// The file in base64, unless it is larger than `max` bytes.
fn read_base64(path: &Path, max: u64) -> Result<String, String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let len = std::fs::metadata(path)
        .map_err(|e| format!("{name}: {e}"))?
        .len();
    if len > max {
        return Err(format!("{name} is larger than {} MB", max / (1024 * 1024)));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_moved_prompt_points_at_files_only_its_old_place_has() {
        let base = std::env::temp_dir().join(format!("arakun-move-{}", std::process::id()));
        let (from, to) = (base.join("main"), base.join("wt"));
        std::fs::create_dir_all(from.join("shots")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        for f in ["shots/a b.png", "new.png", "both.rs"] {
            std::fs::write(from.join(f), "").unwrap();
        }
        std::fs::write(to.join("both.rs"), "").unwrap();
        let text = "see @new.png, @\"shots/a b.png\" and @both.rs; not @gone.png or a@b.c";
        let full = |f: &str| mention(&from.join(f).to_string_lossy());
        assert_eq!(
            move_mentions(text, &from, &to),
            format!(
                "see {}, {} and @both.rs; not @gone.png or a@b.c",
                full("new.png"),
                full("shots/a b.png")
            )
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    use super::*;

    #[test]
    fn finds_mentions() {
        assert_eq!(
            mentions("look at @a.png, and @\"my shot.jpg\" plus a@b.com and @a.png"),
            vec!["a.png", "my shot.jpg"]
        );
        assert_eq!(mentions("@ alone and @\"unclosed"), Vec::<String>::new());
    }

    #[test]
    fn a_title_never_holds_a_secret() {
        let key = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        // In a line with words, the secret is replaced.
        assert_eq!(
            title(&format!("deploy with {key} please"), 80),
            "deploy with [secret] please"
        );
        // A line that is only a secret is skipped.
        assert_eq!(
            title(&format!("{key}\nset up the deploy"), 80),
            "set up the deploy"
        );
        assert_eq!(
            title(&format!("TOKEN={key}\nset up the deploy"), 80),
            "TOKEN=[secret]"
        );
        assert_eq!(
            title(&wrap_paste(&format!("{key}\nthe log")), 80),
            "the log"
        );
        // Nothing but secrets: a generic title.
        assert_eq!(title(key, 80), UNTITLED);
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(title(pem, 80), UNTITLED);
        // A handle saved in a secret's place is not a title either.
        assert_eq!(
            title("deploy with secret-ref:0a1b2c3d, then tag", 80),
            "deploy with [secret], then tag"
        );
        assert_eq!(title("secret-ref:0a1b2c3d", 80), UNTITLED);
        // A secret is replaced before the title is cut, so none of it is left.
        let long = format!("{} {key}", "x".repeat(70));
        assert_eq!(title(&long, 80), format!("{} [secret]", "x".repeat(70)));
        assert_eq!(title("", 80), "");
        assert_eq!(title("  \n ", 80), "");
    }

    #[test]
    fn pastes_fold_out_of_the_prompt() {
        let prompt = format!(
            "fix this\n\n{}\n\n{}",
            wrap_paste("a\nb\n"),
            wrap_paste("c")
        );
        let (typed, pastes) = split_pastes(&prompt);
        assert_eq!(typed, "fix this");
        assert_eq!(pastes, ["a\nb", "c"]);
        assert_eq!(title(&wrap_paste("\nfirst line\nsecond"), 80), "first line");
        assert_eq!(split_pastes("plain"), ("plain".to_string(), Vec::new()));
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(mention("src/a.png"), "@src/a.png");
        assert_eq!(mention("my shot.png"), "@\"my shot.png\"");
        assert_eq!(mentions(&mention("my shot.png")), vec!["my shot.png"]);
    }

    #[test]
    fn a_tool_image_is_saved_once_per_session_as_a_paste() {
        let dir = std::env::temp_dir().join(format!("arakun-tool-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = base64::engine::general_purpose::STANDARD.encode(b"png bytes");
        let block = json!({ "type": "image", "mimeType": "image/png", "data": data });
        let file = save_tool_image(&dir, "s1", &block).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"png bytes");
        // Named so deleting the session finds it: `paste-<digits>.png`.
        let name = file.file_name().unwrap().to_str().unwrap();
        let digits = name
            .strip_prefix("paste-")
            .unwrap()
            .strip_suffix(".png")
            .unwrap();
        assert!(!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        // Sent again, it is the same file; another session gets its own.
        assert_eq!(save_tool_image(&dir, "s1", &block), Some(file.clone()));
        assert_ne!(save_tool_image(&dir, "s2", &block), Some(file));
        let svg = json!({ "type": "image", "mimeType": "image/svg+xml", "data": data });
        assert_eq!(save_tool_image(&dir, "s1", &svg), None);
        let bad = json!({ "type": "image", "mimeType": "image/png", "data": "not base64!" });
        assert_eq!(save_tool_image(&dir, "s1", &bad), None);
        let big = "A".repeat(MAX_IMAGE_BYTES as usize / 3 * 4 + 8);
        let big = json!({ "type": "image", "mimeType": "image/png", "data": big });
        assert_eq!(save_tool_image(&dir, "s1", &big), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_only_existing_images() {
        let dir = std::env::temp_dir().join(format!("arakun-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.PNG"), b"x").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        let found = images("@a.PNG @b.txt @missing.png", &dir);
        assert_eq!(found, vec![dir.join("a.PNG")]);
        let (text, shown) = split_images(
            "look @a.PNG and @b.txt\n@\"a.PNG\" ok@a.PNG @missing.png",
            &dir,
        );
        assert_eq!(text, "look  and @b.txt\n ok@a.PNG @missing.png");
        assert_eq!(shown, vec![dir.join("a.PNG")]);
        assert_eq!(
            split_images("@a.PNG", &dir),
            (String::new(), vec![dir.join("a.PNG")])
        );
        let block = acp_block(&found[0]).unwrap();
        assert_eq!(block["mimeType"], "image/png");
        assert_eq!(block["data"], "eA==");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pdfs_go_as_documents() {
        let dir = std::env::temp_dir().join(format!("arakun-attach-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.PDF"), b"x").unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        let text = "read @spec.PDF and @a.png, not @gone.pdf";
        assert_eq!(documents(text, &dir), vec![dir.join("spec.PDF")]);
        // A PDF is no image: the image list leaves it, and the other way round.
        assert_eq!(images(text, &dir), vec![dir.join("a.png")]);
        assert_eq!(
            split_documents(text, &dir),
            (
                "read  and @a.png, not @gone.pdf".to_string(),
                vec![dir.join("spec.PDF")]
            )
        );
        assert!(is_attachable(&dir.join("spec.PDF")));
        assert!(!is_attachable(&dir.join("gone.pdf")));
        let block = acp_document_block(&dir.join("spec.PDF")).unwrap();
        assert_eq!(block["type"], "resource");
        assert_eq!(block["resource"]["mimeType"], "application/pdf");
        assert_eq!(block["resource"]["blob"], "eA==");
        assert!(acp_document_block(&dir.join("a.png")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_pdf_over_the_limit_is_not_sent() {
        let dir = std::env::temp_dir().join(format!("arakun-attach-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.pdf");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_PDF_BYTES + 1).unwrap();
        assert_eq!(
            acp_document_block(&big).unwrap_err(),
            "big.pdf is larger than 24 MB"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
