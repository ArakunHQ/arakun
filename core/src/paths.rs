//! Where Arakun keeps its files. Every module asks here, so one rule decides
//! what a second Arakun with its own state (`ARAKUN_STATE`, as `arakun dev`
//! sets it) keeps apart from the person's own.
//!
//! - [`store`]: this instance's own files: settings, transcripts, memory,
//!   history. They sit next to the state file, so `ARAKUN_STATE` moves all of
//!   them at once. Without it, that is the data folder.
//! - [`shared`]: what every instance on the machine shares: caches, installed
//!   agents and tools, the updater's download, `arakun server`'s folder, and
//!   stores whose secrets the keyring keeps under one name per machine (a test
//!   Arakun with its own file there would sign the person's own Arakun out).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The state file: `ARAKUN_STATE`, or `state.json` in the data folder.
pub fn state_file() -> PathBuf {
    state_file_in(std::env::var_os("ARAKUN_STATE"), crate::platform::app_dir())
}

/// The folder this instance's own files are in: the state file's.
pub fn data_dir() -> PathBuf {
    data_dir_in(std::env::var_os("ARAKUN_STATE"), crate::platform::app_dir())
}

/// A file or folder of this instance's own, next to the state file.
pub fn store(name: impl AsRef<Path>) -> PathBuf {
    data_dir().join(name)
}

/// A file or folder every Arakun on this machine shares, in the data folder
/// whatever `ARAKUN_STATE` says.
pub fn shared(name: impl AsRef<Path>) -> PathBuf {
    crate::platform::app_dir().join(name)
}

fn state_file_in(state: Option<OsString>, app_dir: PathBuf) -> PathBuf {
    state
        .map(PathBuf::from)
        .unwrap_or_else(|| app_dir.join("state.json"))
}

fn data_dir_in(state: Option<OsString>, app_dir: PathBuf) -> PathBuf {
    match state {
        // A bare `state.json` has the empty parent: the working directory, as
        // the state file itself is read from there.
        Some(state) => Path::new(&state)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        None => app_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_state_file_everything_is_in_the_data_folder() {
        let app = PathBuf::from("/data/arakun");
        assert_eq!(state_file_in(None, app.clone()), app.join("state.json"));
        assert_eq!(data_dir_in(None, app.clone()), app);
    }

    #[test]
    fn a_state_file_takes_the_instance_files_with_it() {
        let app = PathBuf::from("/data/arakun");
        let state = Some(OsString::from("/tmp/dev/state.json"));
        assert_eq!(
            state_file_in(state.clone(), app.clone()),
            PathBuf::from("/tmp/dev/state.json")
        );
        assert_eq!(data_dir_in(state, app), PathBuf::from("/tmp/dev"));
    }

    #[test]
    fn a_bare_state_file_keeps_them_in_the_working_directory() {
        let app = PathBuf::from("/data/arakun");
        let dir = data_dir_in(Some(OsString::from("state.json")), app.clone());
        assert_eq!(dir.join("voice.json"), PathBuf::from("voice.json"));
        let dir = data_dir_in(Some(OsString::new()), app);
        assert_eq!(dir.join("voice.json"), PathBuf::from("voice.json"));
    }
}
