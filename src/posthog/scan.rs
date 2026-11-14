//! Finds the PostHog project keys (`phc_…`) an Arakun project's apps send
//! events with, so the tab can tell which PostHog project it is. `.env`
//! files are often git-ignored, so ignore files are not followed; heavy
//! directories are skipped by name and the walk is bounded.

use std::path::Path;

/// Directories never worth reading for a key.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".output",
    ".turbo",
    ".vercel",
    ".wrangler",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    "coverage",
    "Pods",
    ".gradle",
];

/// Files whose text may hold a key.
const EXTENSIONS: &[&str] = &[
    "env",
    "js",
    "jsx",
    "ts",
    "tsx",
    "mjs",
    "cjs",
    "json",
    "jsonc",
    "toml",
    "yaml",
    "yml",
    "html",
    "vue",
    "svelte",
    "astro",
    "py",
    "rb",
    "go",
    "rs",
    "swift",
    "kt",
    "java",
    "dart",
    "php",
    "ex",
    "exs",
    "plist",
    "xml",
    "properties",
];

const MAX_FILES: usize = 5000;
const MAX_DEPTH: usize = 8;
const MAX_BYTES: u64 = 512 * 1024;

/// The distinct `phc_` keys under `root`, `.env` files' first.
pub fn keys(root: &Path) -> Vec<String> {
    let walk = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .max_depth(Some(MAX_DEPTH))
        .follow_links(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_some_and(|t| t.is_dir())
                && e.depth() > 0
                && SKIP_DIRS.contains(&name.as_ref()))
        })
        .build();
    let mut env_keys = Vec::new();
    let mut other = Vec::new();
    let mut seen = 0;
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        let env = name.starts_with(".env") || name.ends_with(".env");
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !env && !EXTENSIONS.contains(&ext.as_str())
            || name.ends_with(".lock")
            || name == "package-lock.json"
        {
            continue;
        }
        seen += 1;
        if seen > MAX_FILES {
            break;
        }
        if entry.metadata().is_ok_and(|m| m.len() > MAX_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for k in find(&text) {
            let into = if env { &mut env_keys } else { &mut other };
            if !into.contains(&k) {
                into.push(k);
            }
        }
    }
    for k in other {
        if !env_keys.contains(&k) {
            env_keys.push(k);
        }
    }
    env_keys
}

/// Every `phc_` key in `text`: the prefix plus at least 20 key characters,
/// not glued to a longer word.
pub fn find(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("phc_") {
        let start = from + i;
        from = start + 4;
        if start > 0 && is_key_char(bytes[start - 1]) {
            continue;
        }
        let end = bytes[start + 4..]
            .iter()
            .position(|b| !is_key_char(*b))
            .map_or(bytes.len(), |n| start + 4 + n);
        if end - start - 4 >= 20 {
            let k = text[start..end].to_string();
            if !out.contains(&k) {
                out.push(k);
            }
        }
        from = end.max(from);
    }
    out
}

fn is_key_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "phc_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefg";
    const K2: &str = "phc_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";

    #[test]
    fn finds_keys_in_text() {
        let text = format!(
            "NEXT_PUBLIC_POSTHOG_KEY={K1}\nposthog.init('{K2}', {{}})\nphc_short\nxphc_{}\n{K1}",
            &K2[4..]
        );
        assert_eq!(find(&text), vec![K1.to_string(), K2.to_string()]);
        assert!(find("no keys here, phc_ alone").is_empty());
    }

    #[test]
    fn scans_a_project_skipping_heavy_dirs() {
        let root = std::env::temp_dir().join(format!("arakun-ph-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules/posthog-js")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("node_modules/posthog-js/x.js"), format!("'{K2}'")).unwrap();
        std::fs::write(root.join(".git/config"), K2).unwrap();
        std::fs::write(
            root.join("src/analytics.ts"),
            format!("posthog.init('{K2}')"),
        )
        .unwrap();
        std::fs::write(root.join(".env.local"), format!("POSTHOG_KEY={K1}\n")).unwrap();
        std::fs::write(root.join("logo.png"), K1).unwrap();
        // The .env key comes first, then the one in source; node_modules and .git are skipped.
        assert_eq!(keys(&root), vec![K1.to_string(), K2.to_string()]);
        std::fs::remove_file(root.join("src/analytics.ts")).unwrap();
        assert_eq!(keys(&root), vec![K1.to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }
}
