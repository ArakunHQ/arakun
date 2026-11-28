//! Community widgets: small views anyone can publish, which Arakun runs
//! inside the permissions the user approved. The format and the rules are in
//! `docs/superpowers/specs/2026-11-27-community-widgets-design.md`.
//!
//! - [`manifest`]: `widget.toml`, the needs and how an update's needs
//!   compare with what was allowed.
//! - [`sandbox`]: the script's two engines and the host functions, each
//!   checked against the grant.
//! - [`local`]: the real network and files behind them.
//! - [`blocks`]: what `view` draws with.
//! - [`store`]: install, update, pull and remove, pinned by hash.
//! - [`state`]: refreshing on time, and the state a row shows.

pub mod blocks;
pub mod local;
pub mod manifest;
pub mod sandbox;
pub mod state;
pub mod store;
