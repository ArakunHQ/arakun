//! Keeps views on the design system: a test that reads the sources and fails
//! on a gray, text size or font written by hand where the system has a name
//! for it. The system itself (`theme`, `ui`) and the gallery are exempt.
//!
//! What it allows on purpose: strong fills (`bg(theme::mix(0.8))` for a
//! pressed white button) and borders above the strong role. Text sizes that
//! scale with something (`px(size * 0.4)`) are not literals and pass. Corners,
//! padding and gaps must go through a scale whatever they are made of.

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

/// Files that keep their own corners and spacing: the island process draws a
/// fixed HUD that the app's scales must not move.
const SCALE_EXEMPT: &[&str] = &["src/island_host"];

/// The calls whose pixel argument has to go through a scale: corners through
/// `ui::rad` or `ui::nested`, padding and gaps through `ui::sp`.
const SCALED: &[(&str, &str)] = &[
    (".rounded", "ui::rad"),
    (".gap", "ui::sp"),
    (".p(", "ui::sp"),
    (".px(", "ui::sp"),
    (".py(", "ui::sp"),
    (".pt(", "ui::sp"),
    (".pb(", "ui::sp"),
    (".pl(", "ui::sp"),
    (".pr(", "ui::sp"),
];

/// What the argument of a `SCALED` call may start with. Only this list, so a
/// wrapper around a raw size (a variable, `ui::fixed(px(6.))`) fails too.
/// `bar_inset` and `icon_inset` take no number: they are the two insets that
/// must match a fixed bar height, and nothing else can pass through them.
const SCALED_ARGS: &[&str] = &[
    "ui::sp(",
    "ui::rad(",
    "ui::nested(",
    "ui::bar_inset()",
    "ui::icon_inset()",
    "relative(",
];

/// Corners, padding or gaps that do not go through a scale, so the Corners
/// and Spacing settings would not reach them. `next` is the line after, for
/// an argument that starts there.
fn unscaled(line: &str, next: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (call, fix) in SCALED {
        let mut at = 0;
        while let Some(i) = line[at..].find(call) {
            let before = &line[..at + i];
            at += i + call.len();
            // `..pr(x)` fills a struct and `self.p(x, y)` is a method of its
            // own: neither is a style call.
            if before.ends_with('.') || before.ends_with("self") {
                continue;
            }
            let rest = &line[at..];
            // `.rounded(`, `.rounded_tl(`, `.gap_x(`: skip to the open paren.
            let after = if call.ends_with('(') {
                rest
            } else {
                match rest.find('(') {
                    Some(j)
                        if rest[..j]
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_') =>
                    {
                        &rest[j + 1..]
                    }
                    _ => continue,
                }
            };
            let mut arg = after.trim_start();
            if arg.is_empty() {
                arg = next.trim_start();
            }
            // `.rounded_full()` has no argument to check.
            if arg.starts_with(')') {
                continue;
            }
            let arg = arg.strip_prefix("crate::").unwrap_or(arg);
            if !SCALED_ARGS.iter().any(|ok| arg.starts_with(ok)) {
                found.push(format!(
                    "{} not through a scale: use {fix}(…)",
                    call.trim_end_matches('(')
                ));
            }
        }
    }
    found
}

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
        let scaled = !SCALE_EXEMPT.iter().any(|d| rel.starts_with(d));
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            for p in problems(line) {
                report.push(format!("{rel}:{}: {p}", n + 1));
            }
            if scaled {
                let next = lines.get(n + 1).copied().unwrap_or("");
                for p in unscaled(line, next) {
                    report.push(format!("{rel}:{}: {p}", n + 1));
                }
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
    assert_eq!(unscaled(".rounded(px(8.))", "").len(), 1);
    assert_eq!(unscaled(".rounded_tl(px(4.)).gap_x(px(6.))", "").len(), 2);
    assert_eq!(unscaled(".px(px(12.)).py(px(4.))", "").len(), 2);
    assert!(unscaled(".rounded(crate::ui::rad(8.)).gap(ui::sp(6.))", "").is_empty());
    assert!(unscaled(".rounded_full().h(px(20.)).mx(px(4.))", "").is_empty());
    // A helper that hands a raw size back gets around the scale too.
    assert_eq!(unscaled(".px(ui::fixed(px(6.)))", "").len(), 1);
    assert_eq!(unscaled(".pl(ui::fixed(6.)).gap(gap)", "").len(), 2);
    assert_eq!(unscaled(".rounded(corner(8.))", "").len(), 1);
    assert_eq!(unscaled(".pl(", "    px(6.))").len(), 1);
    // Only the named insets skip the scale, and they take no number.
    assert!(unscaled(".pl(ui::bar_inset()).pr(ui::icon_inset())", "").is_empty());
    assert!(unscaled(".pl(ui::sp(14.).max(inset))", "").is_empty());
    assert!(unscaled(".rounded(ui::nested(ui::RADIUS_XL, 5.))", "").is_empty());
    assert!(unscaled(".pl(", "    crate::ui::sp(6.))").is_empty());
    assert!(unscaled("..pr(\"OPEN\")", "").is_empty());
    assert!(unscaled("let p = self.p(x, y);", "").is_empty());
}
