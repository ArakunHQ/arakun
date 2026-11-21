//! What the Isometric tab asks an agent for: the file a new scene goes in,
//! and the prompt that starts its session.

use std::path::{Path, PathBuf};

/// A request for an agent session. `path` is relative to the environment.
pub enum Ask {
    New {
        path: PathBuf,
        words: String,
    },
    Change {
        path: PathBuf,
        words: String,
        /// Why the scene fails to load, when it does.
        error: Option<String>,
        /// The built-in scene it was copied from, when it was.
        copied_from: Option<String>,
    },
}

/// A file name from free text: its first five words, lowercase ASCII
/// letters and digits joined by `-`, at most 40 characters.
pub fn slug(text: &str) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .map(str::to_ascii_lowercase)
        .collect();
    let mut s = words.join("-");
    s.truncate(40);
    let s = s.trim_matches('-');
    if s.is_empty() {
        "scene".into()
    } else {
        s.into()
    }
}

/// `dir/<base>.rhai`, or the first of `<base>-2.rhai`, `-3`… not on disk.
pub fn free_path(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(format!("{base}.rhai"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{base}-{n}.rhai")))
        .find(|p| !p.exists())
        .expect("some number is free")
}

/// The first prompt of the session. The arakun-iso skill carries the how;
/// this only says which file and what to check.
pub fn prompt(ask: &Ask) -> String {
    match ask {
        Ask::New { path, words } => {
            let path = path.display();
            format!(
                "{words}\n\nUse the arakun-iso skill. Write this as a new scene in {path}. \
                 Check it with `arakun iso check {path}` and look at it with \
                 `arakun iso render {path}` before you finish."
            )
        }
        Ask::Change {
            path,
            words,
            error,
            copied_from,
        } => {
            let path = path.display();
            let mut p = format!(
                "{words}\n\nUse the arakun-iso skill. Change the scene in {path} in place and \
                 keep its file name. Check it with `arakun iso check {path}` and look at it \
                 with `arakun iso render {path}` before you finish."
            );
            if let Some(name) = copied_from {
                p.push_str(&format!(
                    "\n\nIt is a copy of the built-in \"{name}\" scene."
                ));
            }
            if let Some(error) = error {
                p.push_str(&format!("\n\nIt fails to load right now:\n{error}"));
            }
            p
        }
    }
}

/// The session's title: "Scene: " and the first six words.
pub fn title(ask: &Ask) -> String {
    let (Ask::New { words, .. } | Ask::Change { words, .. }) = ask;
    let words: Vec<&str> = words.split_whitespace().take(6).collect();
    format!("Scene: {}", words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(
            slug("Our Deploy pipeline, as a diagram!"),
            "our-deploy-pipeline-as-a"
        );
    }

    #[test]
    fn slug_falls_back_to_scene() {
        assert_eq!(slug("🙂 !!!"), "scene");
        assert_eq!(slug(""), "scene");
    }

    #[test]
    fn slug_caps_length() {
        let s = slug(&"a".repeat(100));
        assert!(!s.is_empty() && s.len() <= 40 && !s.ends_with('-'));
    }

    #[test]
    fn free_path_skips_taken_names() {
        let d = std::env::temp_dir().join(format!("arakun-ask-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("cpu.rhai"), "").unwrap();
        assert_eq!(free_path(&d, "cpu"), d.join("cpu-2.rhai"));
        std::fs::write(d.join("cpu-2.rhai"), "").unwrap();
        assert_eq!(free_path(&d, "cpu"), d.join("cpu-3.rhai"));
        assert_eq!(free_path(&d, "db"), d.join("db.rhai"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_prompt_names_the_file() {
        let p = prompt(&Ask::New {
            path: "iso/rack.rhai".into(),
            words: "a small server rack".into(),
        });
        assert!(p.starts_with("a small server rack"));
        assert!(p.contains("arakun-iso skill"));
        assert!(p.contains("new scene in iso/rack.rhai"));
        assert!(p.contains("arakun iso check iso/rack.rhai"));
    }

    #[test]
    fn change_prompt_carries_error_and_origin() {
        let p = prompt(&Ask::Change {
            path: "iso/cpu.rhai".into(),
            words: "bigger".into(),
            error: Some("line 3: oops".into()),
            copied_from: Some("CPU".into()),
        });
        assert!(p.starts_with("bigger"));
        assert!(p.contains("in place"));
        assert!(p.contains("line 3: oops"));
        assert!(p.contains("\"CPU\""));
    }

    #[test]
    fn title_is_short() {
        let t = title(&Ask::New {
            path: "iso/x.rhai".into(),
            words: "one two three four five six seven".into(),
        });
        assert_eq!(t, "Scene: one two three four five six");
    }
}
