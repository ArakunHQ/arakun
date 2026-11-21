//! The agent's sandbox and its broker. The app's headless sessions (`src/headless.rs`)
//! run in them; sessions elsewhere run outside both.

pub mod broker;
pub mod sandbox;

use std::path::PathBuf;

/// Where the app keeps its headless sessions. The sandbox hides it from agents.
pub fn root() -> PathBuf {
    crate::platform::app_dir().join("headless")
}
