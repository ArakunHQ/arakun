//! Keeps views on the design system: a test that reads the sources and fails
//! on a gray, text size or font written by hand where the system has a name
//! for it. The system itself (`theme`, `ui`) and the gallery are exempt.
//!
//! What it allows on purpose: strong fills (`bg(theme::mix(0.8))` for a
//! pressed white button) and borders above the strong role. Sizes that scale
//! with something (`px(size * 0.4)`) are not literals and pass.

use std::path::{Path, PathBuf};

/// Files that define the system or show it off, and the island: a black HUD
/// over the desktop with type of its own, the same whatever the app's scale.
const EXEMPT: &[&str] = &[
    "src/theme.rs",
    "src/ui.rs",
    "src/ui/kit.rs",
    "src/ui/chart.rs",
    "src/gallery_view.rs",
    "src/design_lint.rs",
    "src/island.rs",
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The number in `head<number>)` at each place `head` appears in `line`.
fn numbers_after(line: &str, head: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find(head) {
        rest = &rest[i + head.len()..];
        let n: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(v) = n.parse::<f32>()
            && rest[n.len()..].starts_with(')')
        {
            out.push(v);
        }
    }
    out
}

/// What is wrong with a line, if anything.
fn problems(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for a in numbers_after(line, ".text_color(theme::mix(") {
        if a >= 0.2 {
            found.push(format!(
                "text color mix({a}): use theme::text / text_muted / text_subtle / text_faint"
            ));
        }
    }
    for a in numbers_after(line, ".border_color(theme::mix(") {
        if a <= 0.2 {
            found.push(format!(
                "border mix({a}): use theme::border_subtle / border / border_strong"
            ));
        }
    }
    for a in numbers_after(line, ".bg(theme::mix(") {
        if a <= 0.13 {
            found.push(format!(
                "background mix({a}): use theme::fill_raised / fill_hover / fill_selected"
            ));
        }
    }
    for v in numbers_after(line, ".text_size(px(") {
        found.push(format!("text size {v}: use ui::TEXT_* or .ty(Type::…)"));
    }
    for font in [
        "\"Geist\"",
        "\"Geist Mono\"",
        "\"Adwaita Sans\"",
        "\"JetBrainsMono",
    ] {
        if line.contains(".font_family(") && line.contains(font) {
            found.push(format!("font {font}: use theme::FONT_SANS / FONT_MONO"));
        }
    }
    found
}

#[test]
fn views_use_the_design_system() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    files.sort();
    let mut report = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            for p in problems(line) {
                report.push(format!("{rel}:{}: {p}", n + 1));
            }
        }
    }
    assert!(
        report.is_empty(),
        "{} hand-written styles:\n{}",
        report.len(),
        report.join("\n")
    );
}

#[test]
fn the_lint_catches_what_it_should() {
    assert_eq!(problems(".text_color(theme::mix(0.45))").len(), 1);
    assert_eq!(problems(".border_color(theme::mix(0.08))").len(), 1);
    assert_eq!(problems(".bg(theme::mix(0.04))").len(), 1);
    assert_eq!(problems(".text_size(px(12.))").len(), 1);
    assert_eq!(problems(".font_family(\"Geist\")").len(), 1);
    assert!(problems(".bg(theme::mix(0.8))").is_empty());
    assert!(problems(".text_size(px(ui::TEXT_SM))").is_empty());
    assert_eq!(problems(".text_size(px(18.))").len(), 1);
    assert!(problems(".text_size(px(size * 0.4))").is_empty());
    assert!(problems(".text_color(theme::text_subtle())").is_empty());
}
