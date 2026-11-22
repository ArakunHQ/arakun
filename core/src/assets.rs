use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../assets"]
pub struct Assets;

/// The bundled fonts (Geist and the rest): they are rarely installed.
pub fn load_fonts(cx: &mut gpui::App) {
    let fonts: Vec<Cow<'static, [u8]>> = Assets::iter()
        .filter(|p| p.ends_with(".ttf"))
        .filter_map(|p| Assets::get(&p))
        .map(|f| f.data)
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("arakun: fonts: {e}");
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter(|p| p.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}
