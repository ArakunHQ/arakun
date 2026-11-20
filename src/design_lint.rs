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

/// The calls whose pixel argument has to go through a scale, what to use,
/// and what the argument may start with: corners through the Corners
/// setting, padding and gaps through Spacing. Not the other way round:
/// `.rounded(ui::sp(8.))` would follow the wrong setting.
const SCALED: &[(&str, &str, &[&str])] = &[
    (".rounded", "ui::rad", CORNER_ARGS),
    (".gap", "ui::sp", SPACE_ARGS),
    (".p(", "ui::sp", SPACE_ARGS),
    (".px(", "ui::sp", SPACE_ARGS),
    (".py(", "ui::sp", SPACE_ARGS),
    (".pt(", "ui::sp", SPACE_ARGS),
    (".pb(", "ui::sp", SPACE_ARGS),
    (".pl(", "ui::sp", SPACE_ARGS),
    (".pr(", "ui::sp", SPACE_ARGS),
];

/// What a corner may start with. Only this list, so a wrapper around a raw
/// size (a variable, `corner(8.)`) fails too.
const CORNER_ARGS: &[&str] = &[
    "ui::rad(",
    "ui::nested(",
    // Built from `nested`, so it follows the Corners setting.
    "ui::menu_row_radius()",
];

/// What padding or a gap may start with, for the same reason.
/// `bar_inset`, `icon_inset` and `dock_inset` take no number: they are the
/// insets that must match a fixed bar height, and nothing else can pass
/// through them.
const SPACE_ARGS: &[&str] = &[
    "ui::sp(",
    "ui::bar_inset()",
    "ui::icon_inset()",
    "ui::dock_inset()",
    // Its own setting (Window gap), which Spacing leaves alone on purpose.
    "ui::window_gap_px()",
    "relative(",
];

/// Corners, padding or gaps that do not go through a scale, so the Corners
/// and Spacing settings would not reach them. `next` is the next line with
/// code on it, for an argument that starts there.
fn unscaled(line: &str, next: &str) -> Vec<String> {
    let line = &code_only(line);
    let next = &code_only(next);
    let mut found = Vec::new();
    for (call, fix, allowed) in SCALED {
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
            if arg.is_empty() {
                arg = next.trim_start();
            }
            // `.rounded_full()` has no argument to check.
            if arg.starts_with(')') {
                continue;
            }
            let arg = arg.strip_prefix("crate::").unwrap_or(arg);
            if !allowed.iter().any(|ok| arg.starts_with(ok)) {
                found.push(format!(
                    "{} not through a scale: use {fix}(…)",
                    call.trim_end_matches('(')
                ));
            }
        }
    }
    found.extend(shorthands(line));
    found
}

/// GPUI's fixed-size shorthands (`.rounded_lg()`, `.p_2()`, `.gap_2()`):
/// a size in the name, which no scale reaches. `.rounded_full()` and
/// `.rounded_none()` aren't sizes, and nothing scales zero (`.p_0()`).
fn shorthands(line: &str) -> Vec<String> {
    const SPACE: &[&str] = &[
        "gap_x", "gap_y", "gap", "px", "py", "pt", "pb", "pl", "pr", "p",
    ];
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(i) = line[at..].find('.') {
        let before = &line[..at + i];
        at += i + 1;
        let name: String = line[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if before.ends_with('.')
            || before.ends_with("self")
            || !line[at + name.len()..].starts_with("()")
        {
            continue;
        }
        if let Some(rest) = name.strip_prefix("rounded_") {
            if !rest.ends_with("full") && !rest.ends_with("none") {
                found.push(format!(
                    ".{name}() is a fixed size: use .rounded(ui::rad(…))"
                ));
            }
            continue;
        }
        let size = SPACE
            .iter()
            .find_map(|p| name.strip_prefix(p)?.strip_prefix('_'));
        if let Some(size) = size
            && size.starts_with(|c: char| c.is_ascii_digit())
            && size != "0"
        {
            found.push(format!(".{name}() is a fixed size: use ui::sp(…)"));
        }
    }
    found
}

/// `line` without its comment, and with the inside of its string and char
/// literals blanked: `".gap(gap)"` is text, not a style call. A string that
/// runs on from an earlier line is not seen; none does in a style chain.
fn code_only(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => break,
            // `/* … */`: gone, so `.gap(/* why */ ui::sp(8.))` reads as
            // `.gap( ui::sp(8.))`. One left open runs to the end of the line.
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                out.push(' ');
            }
            // `r"…"`, `r#"…"#` or `br"…"`: no escapes, ends at a quote and
            // as many #.
            'r' if matches!(chars.peek(), Some('"' | '#'))
                && !out
                    .strip_suffix('b')
                    .unwrap_or(&out)
                    .ends_with(|p: char| p.is_alphanumeric() || p == '_') =>
            {
                let mut hashes = 0;
                while chars.peek() == Some(&'#') {
                    chars.next();
                    hashes += 1;
                }
                if chars.next() != Some('"') {
                    // `r#ident`, a raw identifier.
                    out.push('r');
                    continue;
                }
                out.push_str("\"\"");
                'raw: while let Some(c) = chars.next() {
                    if c == '"' {
                        let mut n = 0;
                        while n < hashes && chars.peek() == Some(&'#') {
                            chars.next();
                            n += 1;
                        }
                        if n == hashes {
                            break 'raw;
                        }
                    }
                }
            }
            '"' => {
                out.push_str("\"\"");
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            // A char literal (`'"'`, `'\''`), not a lifetime (`'a`).
            '\'' => {
                let rest: String = chars.clone().take(3).collect();
                let len = if rest.starts_with('\\') {
                    rest.chars().skip(2).position(|c| c == '\'').map(|_| 3)
                } else if rest.chars().nth(1) == Some('\'') {
                    Some(2)
                } else {
                    None
                };
                match len {
                    Some(n) => {
                        out.push_str("' '");
                        for _ in 0..n {
                            chars.next();
                        }
                    }
                    None => out.push(c),
                }
            }
            _ => out.push(c),
        }
    }
    out
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
    // Code only, so a comment can't trip these and a raw string can't hide.
    let code = &code_only(line);
    // The brand has two faces, so any other name is off-brand and these two
    // are spelled once, in theme.
    // Any call on the line, with a comment or space before its argument.
    let named = code
        .match_indices(".font_family(")
        .any(|(i, m)| code[i + m.len()..].trim_start().starts_with('"'));
    if named {
        found.push("font by name: use theme::FONT_SANS / FONT_MONO".into());
    }
    // A placeholder is text someone has to read, so it needs WCAG's 4.5:1;
    // faint (3:1) is for disabled text and icons.
    let faint = code.contains("theme::text_faint()") || code.contains("theme::mix(");
    if faint && code.contains("placeholder") {
        found.push("placeholder in a faint gray: use theme::text_subtle".into());
    }
    // The brand stops at SemiBold (600); nothing heavier is bundled.
    for weight in ["BOLD", "EXTRA_BOLD", "BLACK"] {
        let name = format!("FontWeight::{weight}");
        // The whole name only: `BOLD` must not match `BOLDER` or the like.
        let whole = code.match_indices(&name).any(|(i, _)| {
            !code[i + name.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        });
        if whole {
            found.push(format!("{name}: use SEMIBOLD or lighter"));
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
    assert_eq!(problems(".font_family(\"Caveat\")").len(), 1);
    assert!(problems(".font_family(theme::FONT_MONO)").is_empty());
    assert_eq!(problems(".font_weight(FontWeight::BOLD)").len(), 1);
    assert_eq!(problems("weight: gpui::FontWeight::EXTRA_BOLD,").len(), 1);
    assert_eq!(problems("let w = FontWeight::BLACK;").len(), 1);
    assert_eq!(problems("FontWeight::BOLD }").len(), 1);
    // A weight alone on its line, as in a call split over lines.
    assert_eq!(problems("                FontWeight::BOLD").len(), 1);
    // Comments don't count; a raw string still does.
    assert!(problems(".flex() // not .font_family(\"Caveat\")").is_empty());
    assert!(problems(".flex() /* FontWeight::BOLD */").is_empty());
    assert_eq!(problems(".font_family(r\"Caveat\")").len(), 1);
    assert_eq!(problems(".font_family(/* reason */ \"Caveat\")").len(), 1);
    assert_eq!(
        problems(".font_family(theme::FONT_SANS).child(div().font_family( \"X\"))").len(),
        1
    );
    assert_eq!(
        problems("(input.placeholder.clone(), theme::mix(0.35))").len(),
        1
    );
    assert_eq!(
        problems("(field.placeholder.clone(), theme::text_faint())").len(),
        1
    );
    assert!(problems("(input.placeholder.clone(), theme::text_subtle())").is_empty());
    assert!(problems(".font_weight(FontWeight::SEMIBOLD)").is_empty());
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
    // Shorthands with a size in their name skip the scales too.
    assert_eq!(unscaled(".rounded_lg().p_2().gap_2()", "").len(), 3);
    assert_eq!(unscaled(".rounded_t_md().gap_x_1p5().px_4()", "").len(), 3);
    assert!(unscaled(".rounded_none().p_0().pr_number()", "").is_empty());
    assert!(unscaled(".rounded_l_full().gap_0()", "").is_empty());
    // A block comment is not code: neither what it hides nor what it holds.
    assert!(unscaled(".gap(/* why */ ui::sp(8.))", "").is_empty());
    assert!(unscaled(".gap(ui::sp(8.)) /* .p(px(4.)) */", "").is_empty());
    assert_eq!(unscaled(".gap(/* why */ px(8.))", "").len(), 1);
    assert!(unscaled(".pl(ui::sp(6.)) /* never closed .gap(px(2.))", "").is_empty());
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
    // A comment or a blank line between the call and its argument.
    let commented = ["        // Why six.", "        ui::sp(6.))"];
    assert!(unscaled(".pl(", next_code(&commented)).is_empty());
    assert!(unscaled(".pl(", next_code(&["", "        ui::sp(6.))"])).is_empty());
    assert!(unscaled(".pl( // why", next_code(&commented)).is_empty());
    assert_eq!(unscaled(".pl(", next_code(&["", "    px(6.))"])).len(), 1);
    assert!(unscaled("..pr(\"OPEN\")", "").is_empty());
    assert!(unscaled("let p = self.p(x, y);", "").is_empty());
    // Text about a style call is not one.
    assert!(unscaled(r#"let example = ".gap(gap)";"#, "").is_empty());
    assert!(unscaled(r##"let example = r#".pl(px(6.))"#;"##, "").is_empty());
    assert!(unscaled(r##"let example = br".gap(gap)";"##, "").is_empty());
    assert!(unscaled("let q = '\"'; // .gap(gap)", "").is_empty());
    assert_eq!(unscaled(r#".child("a").gap(gap).child("b")"#, "").len(), 1);
    assert_eq!(unscaled("let c = '\\''; x.gap(gap)", "").len(), 1);
    assert_eq!(
        unscaled("fn f<'a>(x: &'a D) -> D { x.gap(gap) }", "").len(),
        1
    );
    // Each setting's own helper: corners don't follow Spacing, nor gaps Corners.
    assert_eq!(unscaled(".rounded(ui::sp(8.))", "").len(), 1);
    assert_eq!(unscaled(".gap(ui::rad(8.))", "").len(), 1);
    assert_eq!(unscaled(".pl(ui::menu_row_radius())", "").len(), 1);
    assert_eq!(unscaled(".rounded(ui::bar_inset())", "").len(), 1);
    assert!(
        unscaled(
            ".rounded(ui::menu_row_radius()).gap(ui::window_gap_px())",
            ""
        )
        .is_empty()
    );
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
    // good.json also has a full-height wrapper that draws nothing in a
    // bordered box, around a child that fits: no edge of it is cut.
    assert!(!text.contains("src/framed.rs"), "{text}");
    // It also has a switch that is on: the thumb sits at the far end, as far
    // from that side as from the top, so its gap to the left is no inset.
    assert!(!text.contains("src/switch.rs"), "{text}");

    let (code, text) = run("bad.json");
    assert_eq!(code, 1, "{text}");
    for kind in [
        "src/card.rs:20 [radius]",
        "src/bar.rs:9 [inset]",
        // A round icon button is exempt from corner matching, not from insets.
        "src/pill.rs:9 [inset]",
        "src/field.rs:7 [clip]",
        // A border on top only cuts a full-height child too.
        "src/rule.rs:8 [clip]",
        // A square bar frames its chips as much as a rounded one.
        "src/squarebar.rs:9 [inset]",
        // A pill-shaped bar skips the corner check, not the inset one.
        "src/pillbar.rs:9 [inset]",
        // A wrapper that draws nothing doesn't hide a cut child.
        "src/wrapped.rs:9 [clip]",
        // One rounded corner is no pill: its corner still has to match.
        "src/tab.rs:5 [radius]",
        // A thumb near the far end, but not as close as to the top, is still off.
        "src/offswitch.rs:9 [inset]",
    ] {
        assert!(text.contains(kind), "missing {kind}:\n{text}");
    }

    // A border can't cut a child that lies beside its box, not under it.
    let (code, text) = run("disjoint.json");
    assert_eq!(code, 0, "{text}");

    // Nothing to check must not read as nothing wrong. A drawn box of no
    // size draws nothing either.
    for empty in ["empty.json", "blank.json", "zero.json"] {
        let (code, text) = run(empty);
        assert_eq!(code, 2, "{empty}: {text}");
        assert!(text.contains("no drawn boxes"), "{text}");
    }
    let (code, _) = layout_audit(&[]).unwrap();
    assert_eq!(code, 2);

    // A box without its fields is no layout dump, even if it says it is drawn.
    let (code, text) = run("malformed.json");
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("not a layout box"), "{text}");

    // Parents that loop would never reach the root.
    let (code, text) = run("cycle.json");
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("is its own ancestor"), "{text}");
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

    // A frame that parses but isn't a layout dump is not copied.
    touch(&frame, br#"[{"bg": true}]"#, 0);
    let (code, text) = snap(&[]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("not a layout box"), "{text}");

    touch(&frame, &good, 0);
    let (code, text) = snap(&[]);
    assert_eq!(code, 0, "{text}");

    // A step that changed nothing on screen.
    let (code, text) = snap(&["--differs-from".as_ref(), copy.as_os_str()]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("same boxes"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}
