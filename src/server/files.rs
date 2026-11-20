//! The files of the server's folders, for a paired app to browse: a folder's
//! listing (`list_dir`), a file's text (`read_file`) and every file name for
//! `@` mentions (`list_files`). A device names what it wants relative to one
//! of the server's folders, and nothing outside that folder is read.
//!
//! Each answer is built off the main thread: a large repository or a slow
//! disk holds up nothing else.

use std::path::Path;

use serde_json::{Value, json};

use crate::explorer;

/// `dir` of `place`, as the Explorer lists it: `dir` with its `entries`, or
/// with an `error`.
pub fn list_dir(place: &Path, dir: &str) -> Value {
    let listed = explorer::inside(place, dir)
        .ok_or_else(|| "that folder is not inside the environment".to_string())
        .and_then(|path| {
            if path.is_dir() {
                Ok(explorer::list_dir(&path))
            } else {
                Err(format!("{} is not a folder", path.display()))
            }
        });
    let mut msg = json!({ "type": "dir", "place": place, "dir": dir });
    match listed {
        Ok(entries) => {
            msg["entries"] = entries
                .iter()
                .map(|e| json!({ "name": e.name.as_ref(), "dir": e.is_dir, "ignored": e.ignored }))
                .collect();
        }
        Err(e) => msg["error"] = json!(e),
    }
    msg
}

/// The text of `path` in `place`: `text`, or an `error` (too large, binary,
/// not there).
pub fn read_file(place: &Path, path: &str) -> Value {
    let read = explorer::inside(place, path)
        .ok_or_else(|| "that file is not inside the environment".to_string())
        .and_then(|file| explorer::read_text(&file));
    let mut msg = json!({ "type": "file", "place": place, "path": path });
    match read {
        Ok(text) => msg["text"] = json!(text),
        Err(e) => msg["error"] = json!(e),
    }
    msg
}

/// The files of `place` that `@` completes, as git lists them: tracked and
/// untracked, not ignored. None outside a repository.
pub fn list_files(place: &Path) -> Value {
    let files = crate::git::ls_files(place).unwrap_or_default();
    json!({ "type": "files", "place": place, "files": files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arakun-server-files-{}-{}",
            std::process::id(),
            crate::servers::new_id()
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("README.md"), "# Notes\n").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2]).unwrap();
        dir
    }

    #[test]
    fn a_folder_is_listed_folders_first() {
        let place = folder();
        let msg = list_dir(&place, "");
        assert_eq!(msg["type"], "dir");
        assert_eq!(msg["dir"], "");
        let names: Vec<&str> = msg["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["src", "blob.bin", "README.md"]);
        assert_eq!(msg["entries"][0]["dir"], true);
        let inner = list_dir(&place, "src");
        assert_eq!(inner["entries"][0]["name"], "main.rs");
        assert!(list_dir(&place, "nope")["error"].is_string());
        let _ = std::fs::remove_dir_all(place);
    }

    #[test]
    fn a_file_is_sent_as_text_unless_it_cannot_be() {
        let place = folder();
        assert_eq!(read_file(&place, "src/main.rs")["text"], "fn main() {}\n");
        assert_eq!(read_file(&place, "blob.bin")["error"], "Binary file");
        assert!(read_file(&place, "gone.txt")["error"].is_string());
        let _ = std::fs::remove_dir_all(place);
    }

    #[test]
    fn nothing_outside_the_folder_is_read() {
        let place = folder();
        let outside = read_file(&place, "../outside.txt");
        assert_eq!(outside["error"], "that file is not inside the environment");
        assert!(outside.get("text").is_none());
        assert!(read_file(&place, "/etc/hostname")["error"].is_string());
        assert!(list_dir(&place, "..")["entries"].is_null());
        let _ = std::fs::remove_dir_all(place);
    }
}
