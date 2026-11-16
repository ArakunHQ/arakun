//! The phone's page, embedded in the binary. On its own because the folder is
//! named from the package it is built in, and `arakun-server` is another one.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "phone"]
pub struct Page;
