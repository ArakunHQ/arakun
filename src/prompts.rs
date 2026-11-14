//! Saved prompts: text kept for reuse, inserted from the composer with `!`.
//! Stored in `prompts.json` next to arakun's state.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedPrompt {
    pub name: String,
    pub text: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub uses: u32,
    /// Unix seconds.
    #[serde(default)]
    pub last_used: u64,
}

fn file() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("prompts.json"),
        None => crate::state::data_dir().join("prompts.json"),
    }
}

pub fn load() -> Vec<SavedPrompt> {
    std::fs::read(file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn store(prompts: &[SavedPrompt]) {
    let path = file();
    if let Err(e) = crate::durable::write(&path, &serde_json::to_vec_pretty(prompts).unwrap_or_default()) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

/// A short name from the prompt's first words: `review-the-diff-for`.
pub fn name_for(text: &str, taken: &[SavedPrompt]) -> String {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .take(4)
        .collect();
    let base = if words.is_empty() { "prompt".to_string() } else { words.join("-") };
    let mut name = base.clone();
    let mut n = 2;
    while taken.iter().any(|p| p.name == name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// Indices of the prompts matching `query`, best first: pinned, then names
/// starting with it, then names or text containing it; ties by use.
pub fn ranked(prompts: &[SavedPrompt], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut scored: Vec<(u8, usize)> = prompts
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let name = p.name.to_lowercase();
            let score = if q.is_empty() || name.starts_with(&q) {
                0
            } else if name.contains(&q) {
                1
            } else if p.text.to_lowercase().contains(&q) {
                2
            } else {
                return None;
            };
            Some((score, i))
        })
        .collect();
    scored.sort_by(|a, b| {
        let (pa, pb) = (&prompts[a.1], &prompts[b.1]);
        pb.pinned
            .cmp(&pa.pinned)
            .then(a.0.cmp(&b.0))
            .then(pb.uses.cmp(&pa.uses))
            .then(pb.last_used.cmp(&pa.last_used))
            .then_with(|| pa.name.cmp(&pb.name))
    });
    scored.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, text: &str, pinned: bool, uses: u32) -> SavedPrompt {
        SavedPrompt { name: name.into(), text: text.into(), pinned, uses, last_used: 0 }
    }

    #[test]
    fn ranks_pinned_then_matches_then_use() {
        let list = vec![p("review", "look at the diff", false, 1), p("tests", "write tests", false, 9), p("ship", "commit and push", true, 0)];
        assert_eq!(ranked(&list, ""), [2, 1, 0]);
        assert_eq!(ranked(&list, "re"), [0]);
        assert_eq!(ranked(&list, "push"), [2]);
        assert!(ranked(&list, "zzz").is_empty());
    }

    #[test]
    fn names_from_first_words() {
        let taken = vec![p("fix-the-failing-test", "", false, 0)];
        assert_eq!(name_for("Fix the failing test, please", &taken), "fix-the-failing-test-2");
        assert_eq!(name_for("¿?", &[]), "prompt");
    }
}
