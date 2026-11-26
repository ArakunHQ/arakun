//! Site icons: which of a page's icon URLs to try, and a small cache of the icons
//! that loaded, by site, so a tab whose page is not running (after a restart, a page
//! starts when its tab is first shown) still shows its site's icon.
//!
//! The cache is PNG files under `cache/favicons` in arakun's data directory, at most
//! [`MAX_FILES`] of them, plus the ones in use in memory. Nothing here needs CEF.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, RenderImage};

/// Icon URLs tried for one page at most, the `/favicon.ico` fallback included: a page
/// that lists dozens of sizes must not start dozens of downloads.
pub const MAX_CANDIDATES: usize = 6;

/// Icons kept on disk; the oldest go first.
pub const MAX_FILES: usize = 400;

/// Icons kept in memory.
const MAX_MEMORY: usize = 256;

/// The biggest icon side kept, in pixels. A tab draws 14 points, so even at 3x this
/// leaves room, while a page that hands out a huge image does not fill the cache.
const MAX_SIDE: u32 = 128;

/// The site a URL belongs to, as `scheme://host:port` with the port always written:
/// `https://Example.com/a?b` and `https://example.com:443/` are the same site.
/// Only web pages have one: `about:blank`, `data:` and `file:` give `None`.
pub fn origin_key(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        let (inside, after) = v6.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (format!("[{}]", inside.to_ascii_lowercase()), port)
    } else {
        match host_port.rsplit_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), Some(p)),
            None => (host_port.to_ascii_lowercase(), None),
        }
    };
    if host.is_empty() || host == "[]" {
        return None;
    }
    let port = match port {
        None | Some("") => default_port,
        Some(p) => p.parse::<u16>().ok()?,
    };
    Some(format!("{scheme}://{host}:{port}"))
}

/// The icon URLs to try for a page, in order: the ones the page lists, bitmaps before
/// SVGs (Chromium's image download does not draw SVG), then the site's
/// `/favicon.ico`, each once and at most [`MAX_CANDIDATES`].
pub fn candidates(icons: &[String], page: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let (bitmaps, svgs): (Vec<&String>, Vec<&String>) = icons
        .iter()
        .filter(|u| !u.trim().is_empty())
        .partition(|u| !is_svg(u));
    for url in bitmaps.into_iter().chain(svgs) {
        if !out.contains(url) {
            out.push(url.clone());
        }
    }
    out.truncate(MAX_CANDIDATES - 1);
    if let Some(fallback) = fallback_icon(page)
        && !out.contains(&fallback)
    {
        out.push(fallback);
    }
    out
}

/// `<scheme>://<host>[:port]/favicon.ico` for a web page.
fn fallback_icon(page: &str) -> Option<String> {
    let (scheme, rest) = page.trim().split_once("://")?;
    origin_key(page)?;
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some(format!(
        "{}://{}/favicon.ico",
        scheme.to_ascii_lowercase(),
        host_port.to_ascii_lowercase()
    ))
}

fn is_svg(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("data:") {
        return lower.starts_with("data:image/svg");
    }
    let path = lower.split(['?', '#']).next().unwrap_or("");
    path.ends_with(".svg") || path.ends_with(".svgz")
}

enum Slot {
    /// Being read from disk.
    Loading,
    /// Not on disk, or unreadable: asked again only once a page brings an icon.
    Missing,
    Ready(Arc<RenderImage>),
}

thread_local! {
    // GPUI's app lives on the main thread; so does this.
    static MEMORY: RefCell<HashMap<String, Slot>> = RefCell::new(HashMap::new());
}

/// The cached icon of the site `url` is on. The first ask for a site reads it from
/// disk in the background and redraws the windows once it is there, so this is cheap
/// to call while drawing.
pub fn cached(url: &str, cx: &App) -> Option<Arc<RenderImage>> {
    let key = origin_key(url)?;
    let found = MEMORY.with_borrow_mut(|memory| match memory.get(&key) {
        Some(Slot::Ready(image)) => Some(Some(image.clone())),
        Some(Slot::Loading | Slot::Missing) => Some(None),
        None => {
            memory.insert(key.clone(), Slot::Loading);
            None
        }
    });
    if let Some(found) = found {
        return found;
    }
    let read = cx.background_executor().spawn({
        let key = key.clone();
        async move { load(&dir(), &key) }
    });
    cx.spawn(async move |cx| {
        let loaded = read.await;
        // A page may have brought a newer icon for the site meanwhile.
        let still_wanted = MEMORY.with_borrow(|m| matches!(m.get(&key), Some(Slot::Loading)));
        if !still_wanted {
            return;
        }
        match loaded.and_then(to_image) {
            Some(image) => {
                cx.update(|cx| put(key, Slot::Ready(image), cx));
                cx.refresh();
            }
            None => cx.update(|cx| put(key, Slot::Missing, cx)),
        }
    })
    .detach();
    None
}

/// Keeps the icon a page at `page` showed (BGRA, as the tab draws it) for its site.
/// It goes to disk off the UI thread, and never in redacted mode.
pub fn remember(page: &str, bgra: &[u8], width: u32, height: u32, cx: &mut App) {
    let Some(key) = origin_key(page) else { return };
    if !fits(bgra, width, height) {
        return;
    }
    let same = MEMORY.with_borrow(|m| match m.get(&key) {
        Some(Slot::Ready(image)) => {
            let size = image.size(0);
            (size.width.0 as u32, size.height.0 as u32) == (width, height)
                && image.as_bytes(0) == Some(bgra)
        }
        _ => false,
    });
    if same {
        return;
    }
    let icon = Icon {
        bgra: bgra.to_vec(),
        width,
        height,
    };
    if !crate::redact::on() {
        let stored = Icon {
            bgra: icon.bgra.clone(),
            width,
            height,
        };
        let key = key.clone();
        cx.background_executor()
            .spawn(async move {
                let dir = dir();
                if let Err(e) = store(&dir, &key, &stored) {
                    eprintln!("arakun: could not keep a site icon: {e}");
                }
                prune(&dir, MAX_FILES);
            })
            .detach();
    }
    if let Some(image) = to_image(icon) {
        put(key, Slot::Ready(image), cx);
    }
}

/// Puts `slot` in memory, dropping the image it replaces, and an old one when
/// memory is full.
fn put(key: String, slot: Slot, cx: &mut App) {
    let dropped = MEMORY.with_borrow_mut(|memory| {
        let mut dropped = Vec::new();
        if memory.len() >= MAX_MEMORY && !memory.contains_key(&key) {
            let other = memory.keys().find(|k| **k != key).cloned();
            if let Some(Slot::Ready(image)) = other.and_then(|k| memory.remove(&k)) {
                dropped.push(image);
            }
        }
        if let Some(Slot::Ready(image)) = memory.insert(key, slot) {
            dropped.push(image);
        }
        dropped
    });
    for image in dropped {
        cx.drop_image(image, None);
    }
}

fn to_image(icon: Icon) -> Option<Arc<RenderImage>> {
    let buf = image::RgbaImage::from_raw(icon.width, icon.height, icon.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

fn dir() -> PathBuf {
    crate::paths::shared("cache").join("favicons")
}

/// An icon's pixels, BGRA with premultiplied alpha, as the tab draws them.
#[derive(Debug, PartialEq, Eq)]
struct Icon {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
}

fn fits(bgra: &[u8], width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_SIDE
        && height <= MAX_SIDE
        && bgra.len() == width as usize * height as usize * 4
}

/// The file of a site's icon. Every byte but `[a-z0-9.-]` is written as `_xx`, so
/// two sites never share a file and no name holds a path separator.
fn file_name(key: &str) -> Option<String> {
    let mut name = String::with_capacity(key.len() + 4);
    for b in key.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' {
            name.push(b as char);
        } else {
            name.push_str(&format!("_{b:02x}"));
        }
    }
    name.push_str(".png");
    // Most file systems stop at 255 bytes; such a host is not worth a cache file.
    (name.len() <= 200).then_some(name)
}

/// Writes the icon as a PNG. Its pixels go in as they are drawn (premultiplied, with
/// blue and red swapped back to the PNG order), so a load gives the same bytes.
fn store(dir: &Path, key: &str, icon: &Icon) -> std::io::Result<()> {
    let name = file_name(key).ok_or_else(|| std::io::Error::other("site name too long"))?;
    if !fits(&icon.bgra, icon.width, icon.height) {
        return Err(std::io::Error::other("icon too big"));
    }
    let mut rgba = icon.bgra.clone();
    swap_red_blue(&mut rgba);
    let mut png = Vec::new();
    image::RgbaImage::from_raw(icon.width, icon.height, rgba)
        .ok_or_else(|| std::io::Error::other("bad icon size"))?
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(std::io::Error::other)?;
    // The sites someone visits are theirs alone.
    crate::durable::write_private(&dir.join(name), &png)
}

fn load(dir: &Path, key: &str) -> Option<Icon> {
    let bytes = std::fs::read(dir.join(file_name(key)?)).ok()?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    let mut bgra = image.into_raw();
    swap_red_blue(&mut bgra);
    fits(&bgra, width, height).then_some(Icon {
        bgra,
        width,
        height,
    })
}

fn swap_red_blue(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

/// Deletes the oldest icons past `max`.
fn prune(dir: &Path, max: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut icons: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if icons.len() <= max {
        return;
    }
    icons.sort();
    for (_, path) in &icons[..icons.len() - max] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-favicons-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn icon(seed: u8, side: u32) -> Icon {
        let bgra = (0..side * side * 4)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect();
        Icon {
            bgra,
            width: side,
            height: side,
        }
    }

    #[test]
    fn the_site_of_a_url() {
        let key = |u| origin_key(u);
        assert_eq!(
            key("https://example.com/a/b?c=1#d").as_deref(),
            Some("https://example.com:443")
        );
        assert_eq!(key("https://Example.COM:443/"), key("https://example.com"));
        assert_eq!(
            key("http://example.com").as_deref(),
            Some("http://example.com:80")
        );
        assert_ne!(key("http://example.com"), key("https://example.com"));
        assert_eq!(
            key("http://localhost:5173/app").as_deref(),
            Some("http://localhost:5173")
        );
        assert_ne!(key("http://localhost:5173"), key("http://localhost:3000"));
        assert_eq!(
            key("https://user:pw@example.com:8443/x").as_deref(),
            Some("https://example.com:8443")
        );
        assert_eq!(
            key("http://[::1]:8080/").as_deref(),
            Some("http://[::1]:8080")
        );
        assert_eq!(key("http://[::1]/").as_deref(), Some("http://[::1]:80"));
        assert_eq!(
            key("https://example.com?q=a:b").as_deref(),
            Some("https://example.com:443")
        );
        for none in [
            "",
            "about:blank",
            "data:text/html,hi",
            "file:///home/me/a.html",
            "chrome://version",
            "https://",
            "https://example.com:notaport/",
            "https://example.com:99999/",
        ] {
            assert_eq!(key(none), None, "{none}");
        }
    }

    #[test]
    fn bitmaps_come_before_svgs_and_the_site_icon_last() {
        let icons = [
            "https://example.com/icon.svg",
            "https://example.com/icon-32.png",
            "data:image/svg+xml;base64,AAAA",
            "https://example.com/favicon.ico?v=2",
            "https://example.com/icon-32.png",
            "",
        ]
        .map(String::from);
        assert_eq!(
            candidates(&icons, "https://example.com/page?x=1"),
            [
                "https://example.com/icon-32.png",
                "https://example.com/favicon.ico?v=2",
                "https://example.com/icon.svg",
                "data:image/svg+xml;base64,AAAA",
                "https://example.com/favicon.ico",
            ]
        );
    }

    #[test]
    fn a_page_without_icons_tries_its_site_icon_once() {
        assert_eq!(
            candidates(&[], "http://localhost:3000/a"),
            ["http://localhost:3000/favicon.ico"]
        );
        let listed = ["https://example.com/favicon.ico".to_string()];
        assert_eq!(
            candidates(&listed, "https://example.com/"),
            ["https://example.com/favicon.ico"]
        );
        // Pages that are not on a site have nothing to fall back to.
        assert!(candidates(&[], "about:blank").is_empty());
        assert!(candidates(&[], "data:text/html,hi").is_empty());
    }

    #[test]
    fn a_long_icon_list_is_cut_short() {
        let icons: Vec<String> = (0..20)
            .map(|n| format!("https://example.com/icon-{n}.png"))
            .collect();
        let tried = candidates(&icons, "https://example.com/");
        assert_eq!(tried.len(), MAX_CANDIDATES);
        assert_eq!(tried.last().unwrap(), "https://example.com/favicon.ico");
    }

    #[test]
    fn file_names_are_safe_and_distinct() {
        let a = file_name("https://example.com:443").unwrap();
        assert!(!a.contains(['/', '\\', ':']), "{a}");
        assert_ne!(a, file_name("http://example.com:443").unwrap());
        assert_ne!(
            file_name("http://a_b.com:80").unwrap(),
            file_name("http://a.b.com:80").unwrap()
        );
        let long = format!("https://{}.com:443", "a".repeat(250));
        assert_eq!(file_name(&long), None);
    }

    #[test]
    fn an_icon_round_trips_through_disk() {
        let dir = scratch("round-trip");
        let key = "https://example.com:443";
        assert_eq!(load(&dir, key), None);
        let original = icon(7, 32);
        store(&dir, key, &original).unwrap();
        assert_eq!(load(&dir, key), Some(original));
        // Another site does not see it.
        assert_eq!(load(&dir, "http://example.com:80"), None);
        // A newer icon replaces the old one.
        let newer = icon(9, 16);
        store(&dir, key, &newer).unwrap();
        assert_eq!(load(&dir, key), Some(newer));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_huge_or_torn_icon_is_not_kept() {
        let dir = scratch("refuse");
        assert!(store(&dir, "https://a.com:443", &icon(1, MAX_SIDE + 1)).is_err());
        let torn = Icon {
            bgra: vec![0; 10],
            width: 4,
            height: 4,
        };
        assert!(store(&dir, "https://a.com:443", &torn).is_err());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(file_name("https://b.com:443").unwrap()),
            b"not a png",
        )
        .unwrap();
        assert_eq!(load(&dir, "https://b.com:443"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_oldest_icons_go_past_the_bound() {
        let dir = scratch("prune");
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(1000);
        for n in 0..5u64 {
            let key = format!("https://site{n}.com:443");
            store(&dir, &key, &icon(n as u8, 4)).unwrap();
            let file = std::fs::File::options()
                .write(true)
                .open(dir.join(file_name(&key).unwrap()))
                .unwrap();
            file.set_modified(base + std::time::Duration::from_secs(n * 10))
                .unwrap();
        }
        prune(&dir, 3);
        let kept: Vec<bool> = (0..5)
            .map(|n| load(&dir, &format!("https://site{n}.com:443")).is_some())
            .collect();
        assert_eq!(kept, [false, false, true, true, true]);
        // Under the bound nothing goes.
        prune(&dir, 3);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
