//! Keeps secrets out of `{:?}`: a test that reads the sources and fails on a
//! struct or enum that derives `Debug` and keeps a token, a key, a secret or a
//! password in a plain `String`. Such a field takes
//! [`arakun_core::secret::Secret`], which prints as `[redacted]`.
//!
//! A field is judged by its name, so a `key` that is not a secret (a map's
//! key, a fingerprint) says so in a comment on the line above it that starts
//! with `// Not a secret`. The tests at the bottom of a file are skipped.

use std::path::{Path, PathBuf};

/// Words in a field's name, split at `_`, that make it a secret.
const SECRET_WORDS: &[&str] = &[
    "token",
    "tokens",
    "secret",
    "secrets",
    "key",
    "keys",
    "password",
    "passwords",
];

/// The types that print what they hold.
const PLAIN: &[&str] = &["String", "Option<String>", "Vec<String>"];

/// What a comment above a field starts with to keep its plain `String`.
const NOT_A_SECRET: &str = "// Not a secret";

fn secret_name(name: &str) -> bool {
    name.to_ascii_lowercase()
        .split('_')
        .any(|w| SECRET_WORDS.contains(&w))
}

/// The fields of the items in `text` that derive `Debug` and keep a secret in
/// a plain type, as `(line, field)` with lines counted from 1.
fn leaks(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut debug = false;
    let mut test_attr = false;
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if test_attr && t.starts_with("mod tests") {
            break;
        }
        test_attr = t == "#[cfg(test)]";
        if t.starts_with("#[derive(") {
            debug = t.contains("Debug");
        } else if debug && (t.contains("struct ") || t.contains("enum ")) && !t.starts_with("//") {
            debug = false;
            // The item runs to the brace that closes its body.
            let mut depth = 0i32;
            let mut j = i;
            loop {
                let code = code(lines[j]);
                depth += code.matches('{').count() as i32;
                depth -= code.matches('}').count() as i32;
                for field in fields(code) {
                    if !marked(&lines, j) {
                        out.push((j + 1, field));
                    }
                }
                if depth <= 0 || j + 1 == lines.len() {
                    break;
                }
                j += 1;
            }
            i = j;
        } else if !(t.starts_with("#[") || t.starts_with("//") || t.is_empty()) {
            // Anything else between the derive and an item ends it.
            debug = false;
        }
        i += 1;
    }
    out
}

/// `line` without its comment.
fn code(line: &str) -> &str {
    line.split("//").next().unwrap_or(line)
}

/// Whether the comments right above line `n` start with [`NOT_A_SECRET`].
fn marked(lines: &[&str], n: usize) -> bool {
    lines[..n]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.starts_with("//") || l.starts_with("#["))
        .any(|l| l.starts_with(NOT_A_SECRET))
}

/// The names of the secret fields in plain types on one line of an item.
fn fields(code: &str) -> Vec<String> {
    code.split(['{', '}', ',', '('])
        .filter_map(|part| {
            let (name, ty) = part.split_once(':')?;
            if ty.starts_with(':') {
                return None;
            }
            let name = name.split_whitespace().last()?;
            let ty: String = ty.split_whitespace().collect();
            (PLAIN.contains(&ty.as_str()) && secret_name(name)).then(|| name.to_string())
        })
        .collect()
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
fn no_secret_derives_a_debug_that_shows_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "core/src", "server/src"] {
        sources(&root.join(dir), &mut files);
    }
    files.sort();
    let mut report = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "src/secrets_lint.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, field) in leaks(&text) {
            report.push(format!("{rel}:{line}: `{field}`"));
        }
    }
    assert!(
        report.is_empty(),
        "{} fields keep a secret in a plain String of a type that derives Debug. \
         Use arakun_core::secret::Secret, or say why it is not one with a \
         `{NOT_A_SECRET}` comment above it:\n{}",
        report.len(),
        report.join("\n")
    );
}

#[test]
fn the_secrets_lint_catches_what_it_should() {
    let leaky = "\
#[derive(Clone, Debug)]
pub struct Saved {
    pub url: String,
    pub token: String,
    pub admin_token: Option<String>,
}

#[derive(Debug)]
enum Credential {
    ApiKey { key: String },
}
";
    assert_eq!(
        leaks(leaky),
        [
            (4, "token".to_string()),
            (5, "admin_token".to_string()),
            (10, "key".to_string())
        ]
    );
    let fine = "\
#[derive(Clone, Debug)]
pub struct Saved {
    pub token: Secret,
    pub tokens: HashMap<String, Secret>,
    // Not a secret: the token's fingerprint.
    pub key: String,
    pub keyboard: String,
}

#[derive(Clone)]
pub struct Hidden {
    pub token: String,
}

#[cfg(test)]
mod tests {
    #[derive(Debug)]
    struct Fixture { token: String }
}
";
    assert!(leaks(fine).is_empty(), "{:?}", leaks(fine));
}
