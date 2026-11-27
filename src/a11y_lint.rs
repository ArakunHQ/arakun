//! Keeps clickable things visible to screen readers: a test that reads the
//! sources and fails on an element with `on_click` but no role. gpui leaves
//! an element without a role out of the accessibility tree, so VoiceOver,
//! Narrator and Orca can't find it or press it.
//!
//! An element passes when its builder chain sets a role (`.role(…)`,
//! `ui::labelled`, `ui::as_switch`) or starts from a function that does
//! (`ui::icon_button`, `ui::button`, `kit::chip`, a view's own row helper).
//! Those functions are found in the sources, so a new one counts at once.
//!
//! Something clickable that is not a control, such as the backdrop that
//! closes a dialog, says why in an `// a11y: …` comment on its chain.
//!
//! The app had many such elements when this lint came in. `a11y_baseline.txt`
//! counts them per file. A file may not get more, and when one gets fewer,
//! the test asks for the lower count: `ARAKUN_BLESS=1 cargo test
//! --no-default-features --bin arakun a11y_lint` writes it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

const BASELINE: &str = "src/a11y_baseline.txt";

/// What sets a role on the element it is called on, or is called with.
const ROLE_SETTERS: &[&str] = &[".role(", "labelled(", "as_switch("];

/// The builder methods that hand the element to a closure (`|d| d.on_click`),
/// so the element is the one the method is called on.
const PASS_THROUGH: &[&str] = &["when", "when_some", "when_none", "when_else", "map", "tap"];

/// `text` with comments, strings and char literals blanked to spaces, the
/// same length and with the same line breaks, so a byte offset in it is one
/// in `text`: `"(x"` is not a paren, and `// .role(` is not a role.
fn blank(text: &str) -> Vec<u8> {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let wipe = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to.min(b.len())] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |n| i + n);
                wipe(&mut out, i, end);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // Rust's block comments nest.
                let (mut depth, mut j) = (0, i);
                while j < b.len() {
                    if b[j..].starts_with(b"/*") {
                        depth += 1;
                        j += 2;
                    } else if b[j..].starts_with(b"*/") {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                wipe(&mut out, i, j);
                i = j;
            }
            // `r"…"`, `r#"…"#`, `br"…"`: no escapes; ends at a quote and as
            // many #. Not `r#ident` and not the end of a word (`for"`).
            b'r' if (i == 0
                || !ident(b[i - 1])
                || (b[i - 1] == b'b' && (i < 2 || !ident(b[i - 2]))))
                && matches!(b.get(i + 1), Some(b'"' | b'#')) =>
            {
                let mut j = i + 1;
                while b.get(j) == Some(&b'#') {
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i = j;
                    continue;
                }
                let hashes = j - i - 1;
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                let end = b[j + 1..]
                    .windows(close.len())
                    .position(|w| w == close.as_slice())
                    .map_or(b.len(), |n| j + 1 + n + close.len());
                wipe(&mut out, j + 1, end.saturating_sub(close.len()));
                i = end;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                wipe(&mut out, i + 1, j);
                i = j + 1;
            }
            // A char literal (`'('`, `'\''`, `'\u{2192}'`), not a lifetime.
            b'\'' => {
                let end = if b.get(i + 1) == Some(&b'\\') {
                    b[i + 2..]
                        .iter()
                        .take(10)
                        .position(|&c| c == b'\'')
                        .map(|n| i + 2 + n)
                } else {
                    // One char, which may be several bytes.
                    let len = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                    (b.get(i + 1 + len) == Some(&b'\'')).then_some(i + 1 + len)
                };
                match end {
                    Some(e) => {
                        wipe(&mut out, i + 1, e);
                        i = e + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Where the code ends: the tests at the bottom of a file don't draw.
fn code_end(text: &str) -> usize {
    text.find("#[cfg(test)]\nmod tests").unwrap_or(text.len())
}

fn word_before(code: &[u8], mut at: usize) -> &str {
    while at > 0 && code[at - 1] == b' ' {
        at -= 1;
    }
    let end = at;
    while at > 0 && (code[at - 1].is_ascii_alphanumeric() || code[at - 1] == b'_') {
        at -= 1;
    }
    std::str::from_utf8(&code[at..end]).unwrap_or("")
}

/// The chain `at` is in: from where the expression starts (after `(`, `,`,
/// `=`, `;`, `{`, `|` or `=>` at its level) to where it ends. Also returns
/// the open paren that starts it, when one does.
fn chain(code: &[u8], at: usize) -> (usize, usize, Option<usize>) {
    let mut depth = 0i32;
    let mut start = 0;
    let mut paren = None;
    let mut i = at;
    while i > 0 {
        let c = code[i - 1];
        match c {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth == 0 => {
                start = i;
                paren = (c == b'(').then_some(i - 1);
                break;
            }
            b'(' | b'[' | b'{' => depth -= 1,
            b';' | b',' | b'=' | b'|' if depth == 0 => {
                start = i;
                break;
            }
            _ => {}
        }
        i -= 1;
    }
    // A closure's body (`|d| d.on_click`, `|d| { … }`): the paren is the one
    // the closure is passed in.
    if start > 0 && matches!(code[start - 1], b'|' | b'{') {
        let mut depth = 0i32;
        for j in (0..start - 1).rev() {
            match code[j] {
                b')' | b']' | b'}' => depth += 1,
                b'(' | b'[' | b'{' if depth == 0 => {
                    paren = (code[j] == b'(').then_some(j);
                    break;
                }
                b'(' | b'[' | b'{' => depth -= 1,
                b';' if depth == 0 => break,
                _ => {}
            }
        }
    }
    let mut depth = 0i32;
    let mut end = code.len();
    for (j, &c) in code.iter().enumerate().skip(at) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => {
                end = j;
                break;
            }
            b')' | b']' | b'}' => depth -= 1,
            b';' | b',' if depth == 0 => {
                end = j;
                break;
            }
            _ => {}
        }
    }
    (start, end, paren)
}

/// The calls of a chain at its own level, `div().id(x).role(r)` →
/// ["div", "id", "role"], with the text of each one's arguments.
fn calls(code: &[u8], from: usize, to: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut open = 0;
    for i in from..to {
        match code[i] {
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    open = i;
                }
                depth += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 && code[i] == b')' {
                    let name = word_before(code, open).to_string();
                    let args = String::from_utf8_lossy(&code[open + 1..i]).into_owned();
                    out.push((name, args));
                }
            }
            _ => {}
        }
    }
    out
}

/// The variable a chain starts from (`row.on_click`, `|d| d.on_click`), if
/// it starts from one rather than a call.
fn root_variable(code: &[u8], from: usize, to: usize) -> Option<String> {
    let s = String::from_utf8_lossy(&code[from..to]);
    let s = s.trim_start();
    let s = s.strip_prefix('>').unwrap_or(s).trim_start();
    let name: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let rest = s[name.len()..].trim_start();
    let starts_lower = name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_');
    (starts_lower && name != "self" && rest.starts_with('.')).then_some(name)
}

/// Whether the element `on_click` at `at` is called on has a role.
fn has_role(code: &[u8], at: usize, makers: &HashSet<String>, hops: u32) -> bool {
    if hops > 6 {
        return false;
    }
    let (from, to, paren) = chain(code, at);
    let calls = calls(code, from, to);
    let named = calls.iter().any(|(name, args)| {
        makers.contains(name)
            || name == "role"
            || ((name == "map" || PASS_THROUGH.contains(&name.as_str()))
                && ROLE_SETTERS.iter().any(|s| args.contains(s)))
    });
    if named {
        return true;
    }
    // A role-setting constructor wrapped in a helper: `wide(ui::segment(…))`.
    if let Some((_, args)) = calls.first()
        && calls.len() > 1
        && makers.iter().any(|m| {
            args.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|w| w == m)
        })
    {
        return true;
    }
    let Some(var) = root_variable(code, from, to) else {
        return false;
    };
    // `.when(x, |d| d.on_click(…))`: the element is the one `.when` is on.
    if let Some(p) = paren
        && PASS_THROUGH.contains(&word_before(code, p))
    {
        return has_role(code, p, makers, hops + 1);
    }
    // `ui::toggle_group(…, |seg, v| seg.on_click(…))`: the kit made `seg`.
    if let Some(p) = paren
        && makers.contains(word_before(code, p))
    {
        return true;
    }
    // `let row = div()…;` and then `row.on_click(…)`, or `row = row.role(…)`.
    let text = String::from_utf8_lossy(&code[..from]);
    let mut best = None;
    for pat in [format!("let {var} ="), format!("let mut {var} =")] {
        if let Some(i) = text.rfind(&pat) {
            best = best.max(Some(i + pat.len()));
        }
    }
    let Some(init) = best else {
        return false;
    };
    let reassigned = text[init..].contains(&format!("{var} = {var}."))
        && ROLE_SETTERS.iter().any(|s| text[init..].contains(s));
    reassigned || has_role(code, init + 1, makers, hops + 1)
}

/// Functions whose body sets a role on what they return: a chain that starts
/// from one has a role. One that returns another such function's element
/// (`button` → `button_with` → `button_lead`) counts too.
fn role_makers(files: &[(String, String)]) -> HashSet<String> {
    let mut bodies: Vec<(String, String)> = Vec::new();
    for (_, text) in files {
        let code = blank(&text[..code_end(text)]);
        let s = String::from_utf8_lossy(&code);
        let mut at = 0;
        while let Some(i) = s[at..].find("fn ") {
            let head = at + i + 3;
            at = head;
            if at > 3 && s[..at - 3].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let name: String = s[head..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let Some(open) = s[head..].find('{').map(|n| head + n) else {
                break;
            };
            if name.is_empty() || !s[head..open].contains("Stateful") {
                continue;
            }
            let mut depth = 0;
            let mut close = s.len();
            for (j, c) in s[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = open + j;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            bodies.push((name, s[open..close].to_string()));
        }
    }
    let mut out: HashSet<String> = bodies
        .iter()
        .filter(|(_, body)| ROLE_SETTERS.iter().any(|r| body.contains(r)))
        .map(|(name, _)| name.clone())
        .collect();
    loop {
        let more: Vec<String> = bodies
            .iter()
            .filter(|(name, body)| !out.contains(name) && returns_one_of(body, &out))
            .map(|(name, _)| name.clone())
            .collect();
        if more.is_empty() {
            return out;
        }
        out.extend(more);
    }
}

/// Whether `body` starts a statement or its tail with a call to one of
/// `makers` (`button_with(…)`, `let b = ui::button(…)`), rather than only
/// passing one to `.child(…)`.
fn returns_one_of(body: &str, makers: &HashSet<String>) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut at = 0;
    while let Some(i) = body[at..].find('(') {
        let open = at + i;
        at = open + 1;
        let name_start = body[..open].trim_end_matches(ident).len();
        if !makers.contains(&body[name_start..open]) {
            continue;
        }
        // Skip a path (`ui::`, `crate::ui::kit::`, `Self::`) or `self.`.
        let mut before = body[..name_start].trim_end();
        loop {
            let t = before
                .strip_suffix("::")
                .or_else(|| before.strip_suffix("self."));
            match t {
                Some(t) => before = t.trim_end_matches(ident).trim_end(),
                None => break,
            }
        }
        if before.is_empty()
            || before.ends_with(['{', ';', '=', '|'])
            || before.ends_with("=>")
            || before.ends_with("return")
        {
            return true;
        }
    }
    false
}

/// Closures in `code` that set a role on what they make (`let row = |end|
/// div().id(…).role(…)`): in that file, a chain that starts by calling one
/// has a role. Only in that file, since their names (`row`) are common.
fn closure_makers(code: &str, makers: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut at = 0;
    while let Some(i) = code[at..].find("let ") {
        let head = at + i + 4;
        at = head;
        let name: String = code[head..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let rest = code[head + name.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        if name.is_empty() || !(rest.starts_with('|') || rest.starts_with("move |")) {
            continue;
        }
        // The closure runs to the `;` that ends the `let`.
        let start = code.len() - rest.len();
        let mut depth = 0i32;
        let mut end = code.len();
        for (j, c) in code[start..].char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ';' if depth == 0 => {
                    end = start + j;
                    break;
                }
                _ => {}
            }
        }
        let body = &code[start..end];
        if ROLE_SETTERS.iter().any(|r| body.contains(r)) || returns_one_of(body, makers) {
            out.insert(name);
        }
    }
    out
}

/// The line of each `on_click` in `text` whose element has no role.
fn roleless(text: &str, makers: &HashSet<String>) -> Vec<usize> {
    let text = &text[..code_end(text)];
    let code = blank(text);
    let s = String::from_utf8_lossy(&code);
    let mut makers = makers.clone();
    let local = closure_makers(&s, &makers);
    makers.extend(local);
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = s[at..].find(".on_click(") {
        let pos = at + i;
        at = pos + 1;
        if has_role(&code, pos, &makers, 0) {
            continue;
        }
        let (from, to, _) = chain(&code, pos);
        // An `// a11y:` comment on the chain or the line above it.
        let line_start = text[..from].rfind('\n').map_or(0, |n| n + 1);
        let above = text[..line_start.saturating_sub(1)]
            .rfind('\n')
            .map_or(0, |n| n + 1);
        if text[above..to.min(text.len())].contains("// a11y:") {
            continue;
        }
        out.push(text[..pos].matches('\n').count() + 1);
    }
    out
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

#[test]
fn clickable_elements_have_a_role() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    sources(&root.join("src"), &mut paths);
    sources(&root.join("core/src"), &mut paths);
    paths.sort();
    let files: Vec<(String, String)> = paths
        .iter()
        .filter(|p| !p.ends_with("a11y_lint.rs"))
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, std::fs::read_to_string(p).ok()?))
        })
        .collect();
    let makers = role_makers(&files);
    let mut found: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (rel, text) in &files {
        let lines = roleless(text, &makers);
        if !lines.is_empty() {
            found.insert(rel.clone(), lines);
        }
    }

    let path = root.join(BASELINE);
    if std::env::var_os("ARAKUN_BLESS").is_some() {
        let mut out = String::from(
            "# Clickable elements without a role, per file (see src/a11y_lint.rs).\n\
             # Only ever lower these: give the element a role instead.\n",
        );
        for (rel, lines) in &found {
            out.push_str(&format!("{rel} {}\n", lines.len()));
        }
        std::fs::write(&path, out).expect("write the baseline");
        return;
    }
    let baseline: BTreeMap<String, usize> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (file, n) = l.rsplit_once(' ')?;
            Some((file.to_string(), n.parse().ok()?))
        })
        .collect();
    let mut more = Vec::new();
    let mut fewer = Vec::new();
    for (rel, lines) in &found {
        let allowed = baseline.get(rel).copied().unwrap_or(0);
        if lines.len() > allowed {
            let at: Vec<String> = lines.iter().map(|n| format!("{rel}:{n}")).collect();
            more.push(format!(
                "{rel}: {} without a role, {allowed} allowed. One of these is new:\n  {}",
                lines.len(),
                at.join("\n  ")
            ));
        }
    }
    for (rel, &allowed) in &baseline {
        let now = found.get(rel).map_or(0, Vec::len);
        if now < allowed {
            fewer.push(format!("{rel}: {now}, was {allowed}"));
        }
    }
    assert!(
        more.is_empty(),
        "Clickable elements that screen readers can't see. Give each a role and a \
         name (`.role(Role::Button).aria_label(…)`, `ui::labelled`), or start it from \
         a kit control that has one (`ui::button`, `ui::icon_button`). If it is not a \
         control, say why in an `// a11y: …` comment on it.\n{}",
        more.join("\n")
    );
    assert!(
        fewer.is_empty(),
        "Fewer clickable elements without a role than {BASELINE} allows. Lower it with \
         `ARAKUN_BLESS=1 cargo test --no-default-features --bin arakun a11y_lint`:\n{}",
        fewer.join("\n")
    );
}

#[cfg(test)]
fn check(src: &str) -> Vec<usize> {
    let files = vec![("x.rs".to_string(), src.to_string())];
    roleless(src, &role_makers(&files))
}

#[test]
fn the_lint_catches_a_click_without_a_role() {
    assert_eq!(check("fn v() { div().id(\"a\").on_click(f); }"), vec![1]);
    // Inside a `.when`, the element is the outer one.
    assert_eq!(
        check("fn v() { div().id(\"a\").when(x, |d| d.on_click(f)); }"),
        vec![1]
    );
    // A role on a child is not a role on the element.
    assert_eq!(
        check("fn v() { div().id(\"a\").child(div().id(\"b\").role(Role::Button)).on_click(f); }"),
        vec![1]
    );
    // A role in a string or a comment is not one either.
    assert_eq!(
        check("fn v() { div().id(\".role(\") // .role(\n.on_click(f); }"),
        vec![2]
    );
}

#[test]
fn the_lint_passes_a_click_with_a_role() {
    let ok = [
        "fn v() { div().id(\"a\").role(Role::Button).on_click(f); }",
        "fn v() { div().id(\"a\").on_click(f).role(Role::Button); }",
        "fn v() { div().id(\"a\").map(|d| ui::labelled(d, Role::Button, \"Go\")).on_click(f); }",
        "fn v() { div().id(\"a\").role(Role::Tab).when(x, |d| d.on_click(f)); }",
        "fn v() { ui::icon_button(\"a\", \"x\", 12., \"Close\").on_click(f); }",
        "fn v() { let row = div().id(\"a\").role(Role::Button); row.on_click(f); }",
        "fn v() { wide(ui::segment(\"a\", None, \"x\", true)).on_click(f); }",
        "fn v() { let row = |e| div().id(\"a\").role(Role::Button).child(e); row(x).on_click(f); }",
        // a11y: a backdrop is not a control.
        "fn v() {\n// a11y: closes the dialog\ndiv().id(\"a\").on_click(f); }",
        // The tests at the bottom of a file don't count.
        "#[cfg(test)]\nmod tests { fn t() { div().id(\"a\").on_click(f); } }",
    ];
    let makers = "fn icon_button() -> Stateful<Div> { labelled(div(), Role::Button, l) }\n\
                  fn segment() -> Stateful<Div> { div().role(Role::RadioButton) }\n";
    for src in ok {
        let full = format!("{makers}{src}");
        assert_eq!(check(&full), Vec::<usize>::new(), "{src}");
    }
}
