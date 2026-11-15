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
    // Built from `nested`, so it follows the Corners setting.
    "ui::menu_row_radius()",
    // Its own setting (Window gap), which Spacing leaves alone on purpose.
    "ui::window_gap_px()",
    "relative(",
];

/// Corners, padding or gaps that do not go through a scale, so the Corners
/// and Spacing settings would not reach them. `next` is the next line with
/// code on it, for an argument that starts there.
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
            // `.gap(` or `.gap( // why`: the argument is on a later line.
            if arg.is_empty() || arg.starts_with("//") {
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

/// The first of `lines` with code on it: an argument on the line after its
/// call can have a comment or a blank line before it.
fn next_code<'a>(lines: &[&'a str]) -> &'a str {
    lines
        .iter()
        .find(|l| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with("//")
        })
        .copied()
        .unwrap_or("")
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
                for p in unscaled(line, next_code(&lines[n + 1..])) {
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
    // An argument on a later line, after a comment or a blank line.
    assert_eq!(unscaled(".gap(", next_code(&["    px(8.)", ")"])).len(), 1);
    let later = ["        // Why eight.", "", "        px(8.))"];
    assert_eq!(unscaled(".gap(", next_code(&later)).len(), 1);
    assert_eq!(unscaled(".gap( // why", next_code(&later)).len(), 1);
    assert!(unscaled(".gap(", next_code(&["", "    crate::ui::sp(8.))"])).is_empty());
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

/// Runs `tools/layout_audit.py` with `args` from the repository root and
/// returns its exit code and output, or `None` when this machine has no Python.
fn layout_audit(args: &[&std::ffi::OsStr]) -> Option<(i32, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("tools/layout_audit.py"))
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &*String::from_utf8_lossy(&out.stderr);
    Some((out.status.code().unwrap_or(-1), text))
}

fn python_missing() -> bool {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipped: python3 is not installed, so the layout audit can't run");
    }
    !ok
}

/// The audit is a script nothing else runs on CI, so its checks are pinned
/// here against dumps in the format our GPUI fork writes.
#[test]
fn the_layout_audit_catches_what_it_should() {
    if python_missing() {
        return;
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/layout-audit-fixtures");
    let run = |name: &str| layout_audit(&[fixtures.join(name).as_os_str()]).unwrap();

    let (code, text) = run("good.json");
    assert_eq!(code, 0, "{text}");

    let (code, text) = run("bad.json");
    assert_eq!(code, 1, "{text}");
    for kind in [
        "src/card.rs:20 [radius]",
        "src/bar.rs:9 [inset]",
        "src/field.rs:7 [clip]",
    ] {
        assert!(text.contains(kind), "missing {kind}:\n{text}");
    }

    // Nothing to check must not read as nothing wrong.
    for empty in ["empty.json", "blank.json"] {
        let (code, text) = run(empty);
        assert_eq!(code, 2, "{empty}: {text}");
        assert!(text.contains("no drawn boxes"), "{text}");
    }
    let (code, _) = layout_audit(&[]).unwrap();
    assert_eq!(code, 2);
}

/// GPUI rewrites the dump in place every frame, so the audit has to refuse a
/// half-written file and a frame from before the step it checks.
#[test]
fn the_layout_audit_refuses_a_partial_or_stale_dump() {
    use std::time::{Duration, SystemTime};
    if python_missing() {
        return;
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/layout-audit-fixtures");
    let good = std::fs::read(fixtures.join("good.json")).unwrap();
    let dir = std::env::temp_dir().join(format!("arakun-layout-audit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let frame = dir.join("frame.json");
    let mark = dir.join("mark");
    let copy = dir.join("copy.json");
    let touch = |path: &Path, bytes: &[u8], ago: u64| {
        std::fs::write(path, bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(ago))
            .unwrap();
    };
    let snap = |extra: &[&std::ffi::OsStr]| {
        let mut args = vec![
            "snap".as_ref(),
            frame.as_os_str(),
            copy.as_os_str(),
            "--newer-than".as_ref(),
            mark.as_os_str(),
            "--timeout".as_ref(),
            "0.2".as_ref(),
        ];
        args.extend_from_slice(extra);
        layout_audit(&args).unwrap()
    };

    let (code, text) = layout_audit(&[frame.as_os_str()]).unwrap();
    assert_eq!(code, 2, "a missing dump: {text}");

    touch(&frame, &good[..good.len() / 2], 0);
    let (code, text) = layout_audit(&[frame.as_os_str()]).unwrap();
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("not whole JSON"), "{text}");

    // A whole frame drawn before the step started.
    touch(&frame, &good, 20);
    touch(&mark, b"", 10);
    let (code, text) = snap(&[]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("older than this step"), "{text}");

    // A half-written frame drawn after it.
    touch(&frame, &good[..good.len() / 2], 0);
    let (code, text) = snap(&[]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("not whole JSON"), "{text}");

    touch(&frame, &good, 0);
    let (code, text) = snap(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(std::fs::read(&copy).unwrap(), good);

    // A step that changed nothing on screen.
    let (code, text) = snap(&["--differs-from".as_ref(), copy.as_os_str()]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("same boxes"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}
