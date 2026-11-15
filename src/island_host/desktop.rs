//! Desktop notifications from some apps (Discord) on the island too. It
//! watches the session bus with `busctl monitor` and reads each `Notify` call
//! as it goes by: swaync still gets them all, for its list. `ARAKUN_NOTICES=0`
//! turns it off; `ARAKUN_NOTICES=discord,slack` picks the apps. A card shows
//! the sender as the app names them and their picture, when it sends one.

use std::hash::{Hash as _, Hasher as _};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::island::{Alert, AlertKind};

const APPS: &str = "discord";
const MATCH: &str = "type='method_call',interface='org.freedesktop.Notifications',member='Notify'";

/// Watches on its own thread; `notice` gets each card to show.
pub fn start(notice: impl Fn(Alert) + Send + 'static) {
    let apps = std::env::var("ARAKUN_NOTICES").unwrap_or_else(|_| APPS.into());
    if apps.trim() == "0" {
        return;
    }
    let apps: Vec<String> = apps
        .split(',')
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .collect();
    let _ = std::thread::Builder::new()
        .name("arakun-notices".into())
        .spawn(move || {
            loop {
                if let Err(e) = watch(&apps, &notice) {
                    eprintln!("arakun: island: notifications: {e}; again in 30 s");
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        });
}

fn watch(apps: &[String], notice: &dyn Fn(Alert)) -> Result<(), String> {
    let mut cmd = Command::new("busctl");
    cmd.args(["--user", "monitor", "--json=short", "--match", MATCH])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt as _;
        // Goes with the host: this thread lives as long as it does.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("busctl: {e}"))?;
    let out = child.stdout.take().ok_or("no output")?;
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        if let Some(alert) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|m| card(&m, apps))
        {
            notice(alert);
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Err(format!("busctl exited with {status}"))
}

/// The card for one `Notify` call (`susssasa{sv}i`: app, replaces, icon,
/// summary, body, actions, hints, timeout), when it comes from one of `apps`.
fn card(msg: &Value, apps: &[String]) -> Option<Alert> {
    let data = msg.pointer("/payload/data")?.as_array()?;
    let text = |i: usize| data.get(i).and_then(Value::as_str).unwrap_or_default();
    let entry = data
        .get(6)
        .and_then(|h| h.pointer("/desktop-entry/data"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let from = |a: &String| {
        text(0).to_lowercase().contains(a.as_str()) || entry.to_lowercase().contains(a.as_str())
    };
    if !apps.iter().any(from) {
        return None;
    }
    let (title, body) = (sender(text(3)), super::brief(text(4)));
    if title.is_empty() && body.is_empty() {
        return None;
    }
    let image = data
        .get(6)
        .and_then(|h| picture(h, text(2)))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(Alert {
        key: "".into(),
        kind: AlertKind::Info,
        title: title.to_string().into(),
        subtitle: body.into(),
        req: "".into(),
        image: image.into(),
        brand: Default::default(),
    })
}

/// The name a sender shows, without where they wrote: Discord titles a
/// server's message "Dani (#general, Arakun)" and a group's "Dani (Amigos)".
fn sender(summary: &str) -> &str {
    let summary = summary.trim();
    match summary.strip_suffix(')').and_then(|s| s.rfind(" (")) {
        Some(open) if open > 0 => &summary[..open],
        _ => summary,
    }
}

/// The notification's picture as a file: its pixels (`image-data`, written
/// once as a PNG), else the file it names (`image-path` or the icon).
fn picture(hints: &Value, icon: &str) -> Option<PathBuf> {
    let hint = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| hints.pointer(&format!("/{n}/data")))
    };
    if let Some(png) = hint(&["image-data", "image_data", "icon_data"]).and_then(pixels) {
        return Some(png);
    }
    let path = hint(&["image-path", "image_path"])
        .and_then(Value::as_str)
        .unwrap_or(icon);
    let path = Path::new(path.strip_prefix("file://").unwrap_or(path));
    (path.is_absolute() && path.is_file()).then(|| path.to_path_buf())
}

/// `(iiibiiay)`: width, height, row stride, alpha, bits per sample, channels
/// and the bytes, saved as a PNG named after them.
fn pixels(d: &Value) -> Option<PathBuf> {
    let d = d.as_array()?;
    let int = |i: usize| d.get(i).and_then(Value::as_u64).map(|n| n as usize);
    let (w, h, stride, channels) = (int(0)?, int(1)?, int(2)?, int(5)?);
    let bytes: Vec<u8> = d
        .get(6)?
        .as_array()?
        .iter()
        .map(|b| b.as_u64().unwrap_or(0) as u8)
        .collect();
    if int(4)? != 8
        || !(channels == 3 || channels == 4)
        || w == 0
        || h == 0
        || bytes.len() < stride * (h - 1) + w * channels
    {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("arakun-notices");
    let file = dir.join(format!("{:016x}.png", hasher.finish()));
    if file.is_file() {
        return Some(file);
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in bytes.chunks(stride).take(h) {
        for px in row[..w * channels].chunks(channels) {
            rgba.extend_from_slice(&[px[0], px[1], px[2], if channels == 4 { px[3] } else { 255 }]);
        }
    }
    let png = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    std::fs::create_dir_all(&dir).ok()?;
    png.save(&file).ok()?;
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notify(app: &str, summary: &str, body: &str) -> Value {
        serde_json::json!({"payload": {"type": "susssasa{sv}i", "data": [app, 0, "", summary, body, [], {}, -1]}})
    }

    #[test]
    fn only_the_apps_asked_for() {
        let apps = vec!["discord".to_string()];
        let alert = card(
            &notify(
                "discord",
                "Dani (#general, Arakun)",
                "ya quedó **el** deploy",
            ),
            &apps,
        )
        .unwrap();
        assert_eq!(alert.title.as_ref(), "Dani");
        assert_eq!(alert.subtitle.as_ref(), "ya quedó el deploy");
        assert!(!alert.buttons());
        assert!(card(&notify("Screenshot", "Captura", "guardada"), &apps).is_none());
        assert!(card(&notify("Discord", "", ""), &apps).is_none());
    }

    #[test]
    fn the_name_without_where() {
        assert_eq!(sender("Dani (#general, Arakun)"), "Dani");
        assert_eq!(sender("Dani (Amigos)"), "Dani");
        assert_eq!(sender("Dani"), "Dani");
        assert_eq!(sender("(sin nombre)"), "(sin nombre)");
    }

    #[test]
    fn pixels_become_a_png() {
        // 2×1 RGB with a padded row.
        let data = serde_json::json!([2, 1, 8, false, 8, 3, [255, 0, 0, 0, 255, 0, 9, 9]]);
        let file = pixels(&data).unwrap();
        let png = image::open(&file).unwrap().to_rgba8();
        assert_eq!(png.get_pixel(1, 0).0, [0, 255, 0, 255]);
    }
}
