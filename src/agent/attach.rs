//! Images attached to a prompt with `@path` mentions (`@"path with spaces"`
//! quotes a path). The text is sent as written; the images go alongside it,
//! as paths for Droid and as base64 blocks for ACP agents.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};

/// The Anthropic API rejects larger images, so they are not worth sending.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

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
    let mut out = String::with_capacity(text.len());
    let mut found: Vec<PathBuf> = Vec::new();
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
            .then(|| resolve(path, cwd))
            .flatten()
            .filter(|p| is_image_file(p));
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

/// What a session title is made from: the typed words, else the first pasted line.
pub fn title_text(text: &str) -> String {
    let (typed, pastes) = split_pastes(text);
    if !typed.is_empty() || pastes.is_empty() {
        return typed;
    }
    pastes[0]
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// An ACP `image` content block, or why the file can't be sent.
pub fn acp_block(path: &Path) -> Result<Value, String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mime = mime_type(path).ok_or_else(|| format!("{name} is not an image"))?;
    let len = std::fs::metadata(path)
        .map_err(|e| format!("{name}: {e}"))?
        .len();
    if len > MAX_IMAGE_BYTES {
        return Err(format!("{name} is larger than 5 MB"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(json!({
        "type": "image",
        "mimeType": mime,
        "data": data,
        "uri": format!("file://{}", path.display()),
    }))
}

#[cfg(test)]
mod tests {
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
    fn pastes_fold_out_of_the_prompt() {
        let prompt = format!(
            "fix this\n\n{}\n\n{}",
            wrap_paste("a\nb\n"),
            wrap_paste("c")
        );
        let (typed, pastes) = split_pastes(&prompt);
        assert_eq!(typed, "fix this");
        assert_eq!(pastes, ["a\nb", "c"]);
        assert_eq!(
            title_text(&wrap_paste("\nfirst line\nsecond")),
            "first line"
        );
        assert_eq!(split_pastes("plain"), ("plain".to_string(), Vec::new()));
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(mention("src/a.png"), "@src/a.png");
        assert_eq!(mention("my shot.png"), "@\"my shot.png\"");
        assert_eq!(mentions(&mention("my shot.png")), vec!["my shot.png"]);
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
}
