//! The code the app shares with `arakun-server` and the examples. Each of them
//! is a thin binary over this crate; what they answer differently, it asks
//! through [`host`].

pub mod accept;
pub mod account;
pub mod agent;
pub mod assets;
pub mod browser;
pub mod caret;
pub mod clone_source;
pub mod cloud;
pub mod durable;
pub mod explorer;
pub mod fetch;
pub mod frame_timer;
pub mod git;
pub mod git_call;
pub mod guard;
pub mod headless;
pub mod highlight;
pub mod host;
pub mod input;
pub mod island;
pub mod mascot;
pub mod model;
pub mod phone;
mod phone_page;
pub mod platform;
pub mod poll;
pub mod redact;
pub mod rotate;
pub mod schedule;
pub mod secret_store;
pub mod secrets;
pub mod server;
pub mod signals;
pub mod terminal;
pub mod theme;
pub mod ui;
pub mod zoom;
