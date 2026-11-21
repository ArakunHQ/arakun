//! The phone's page, embedded in the binary.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../phone"]
pub struct Page;
