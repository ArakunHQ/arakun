//! Whether agents may click, type and run scripts on any page, not only local
//! ones (`arakun page`). Off by default: those tabs carry the user's signed-in
//! sessions. Turned on in Settings → General; stored as a marker file.
//!
//! Kept out of `shell` so Settings can read and set it without depending on the
//! shell that shows it.

use std::path::PathBuf;

pub fn any_page() -> bool {
    marker().is_file()
}

pub fn set_any_page(on: bool) {
    let path = marker();
    if on {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, "agents may drive external pages\n");
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

fn marker() -> PathBuf {
    crate::platform::app_dir().join("drive-any-page")
}
