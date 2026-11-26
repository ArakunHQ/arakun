//! The app's state files are loaded and saved through core's `stored`, which
//! the server uses too.

pub use arakun_core::stored::*;

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    /// An empty directory of its own, for the stores' tests.
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-stored-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What the one copy of `path` set aside holds; `None` without one.
    pub(crate) fn kept(path: &Path) -> Option<String> {
        let name = format!("{}.corrupt-", path.file_name()?.to_string_lossy());
        let copies: Vec<PathBuf> = std::fs::read_dir(path.parent()?)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&name))
            })
            .collect();
        match copies.as_slice() {
            [one] => std::fs::read_to_string(one).ok(),
            _ => None,
        }
    }
}
