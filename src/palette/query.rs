//! What was typed into the palette: a scope prefix (`>` commands, `/` files,
//! `:` settings, `@` people, `#` pull requests), tokens that became chips
//! (`@opus`, a branch name), and the words left to search for.

/// What the list is limited to, from the first character typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Commands,
    Files,
    Settings,
    People,
    Pulls,
}

impl Scope {
    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            '>' => Scope::Commands,
            '/' => Scope::Files,
            ':' => Scope::Settings,
            '@' => Scope::People,
            '#' => Scope::Pulls,
            _ => return None,
        })
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Scope::Commands => ">",
            Scope::Files => "/",
            Scope::Settings => ":",
            Scope::People => "@",
            Scope::Pulls => "#",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Scope::Commands => "Run a command…",
            Scope::Files => "Find a file or symbol…",
            Scope::Settings => "Change a setting…",
            Scope::People => "Find a person…",
            Scope::Pulls => "Find a pull request…",
        }
    }
}

/// A word that turned into a chip before the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chip {
    /// A model to ask, by its id (`claude-opus-5-5`) and how it reads.
    Model { id: String, label: String },
    /// A branch or worktree to work in.
    Branch(String),
}

/// Models one can name with `@`: the word typed, the id, the label.
pub const MODELS: &[(&str, &str, &str)] = &[
    ("opus", "claude-opus-5-5", "Opus 5.5"),
    ("sonnet", "claude-sonnet-5-5", "Sonnet 5.5"),
    ("haiku", "claude-haiku-5-5", "Haiku 5.5"),
    ("fable", "claude-fable-5-2", "Fable 5.2"),
];

/// The model a `@word` names, if any: a prefix of three letters or more.
pub fn model_of(word: &str) -> Option<Chip> {
    let w = word.to_lowercase();
    if w.chars().count() < 3 {
        return None;
    }
    MODELS
        .iter()
        .find(|(name, _, _)| name.starts_with(&w))
        .map(|(_, id, label)| Chip::Model {
            id: id.to_string(),
            label: label.to_string(),
        })
}

/// What `take` found at the start of the input.
#[derive(Debug, PartialEq, Eq)]
pub enum Lead {
    Scope(Scope),
    Chip(Chip),
}

/// Takes what should leave the input from its start: a scope character typed
/// first, or a finished word (one followed by a space) that names a model or
/// a known branch. Returns it and the text left.
pub fn take(text: &str, has_scope: bool, branches: &[String]) -> Option<(Lead, String)> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !has_scope && let Some(scope) = Scope::from_char(first) {
        // `@opus ` is a model, not a person.
        if first == '@'
            && let Some((word, rest)) = text[1..].split_once(' ')
            && let Some(chip) = model_of(word)
        {
            return Some((Lead::Chip(chip), rest.trim_start().to_string()));
        }
        // `/` followed by a path-like word is still a file search, so keep it.
        return Some((Lead::Scope(scope), chars.as_str().trim_start().to_string()));
    }
    let (word, rest) = text.split_once(' ')?;
    if let Some(name) = word.strip_prefix('@')
        && let Some(chip) = model_of(name)
    {
        return Some((Lead::Chip(chip), rest.trim_start().to_string()));
    }
    if is_branch_word(word, branches) {
        return Some((
            Lead::Chip(Chip::Branch(word.to_string())),
            rest.trim_start().to_string(),
        ));
    }
    None
}

/// A branch worth a chip: one the repository has, not the default one (which
/// is where work goes anyway), and long enough not to be an everyday word.
fn is_branch_word(word: &str, branches: &[String]) -> bool {
    word.chars().count() >= 4
        && !matches!(word, "main" | "master")
        && (word.contains('-') || word.contains('/'))
        && branches.iter().any(|b| b == word)
}

/// Whether the words read as a question to answer rather than a search.
pub fn is_question(text: &str) -> bool {
    let t = text.trim();
    if t.ends_with('?') && t.split_whitespace().count() >= 2 {
        return true;
    }
    let first = t
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase()
        .trim_start_matches('¿')
        .to_string();
    const ASK: &[&str] = &[
        "why", "how", "what", "when", "where", "which", "who", "explain", "por", "porqué", "qué",
        "que", "cómo", "como", "cuándo", "dónde", "cuál", "quién",
    ];
    t.split_whitespace().count() >= 3 && ASK.contains(&first.as_str()) || t.starts_with('¿')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branches() -> Vec<String> {
        vec!["main".into(), "phone-local".into(), "lab-knobs".into()]
    }

    #[test]
    fn a_first_character_sets_the_scope() {
        assert_eq!(
            take("> re", false, &[]),
            Some((Lead::Scope(Scope::Commands), "re".into()))
        );
        assert_eq!(
            take("/island card", false, &[]),
            Some((Lead::Scope(Scope::Files), "island card".into()))
        );
        assert_eq!(
            take(":", false, &[]),
            Some((Lead::Scope(Scope::Settings), String::new()))
        );
        assert_eq!(
            take("@ rik", false, &[]),
            Some((Lead::Scope(Scope::People), "rik".into()))
        );
        assert_eq!(
            take("#64", false, &[]),
            Some((Lead::Scope(Scope::Pulls), "64".into()))
        );
        // Only once.
        assert_eq!(take("> re", true, &[]), None);
    }

    #[test]
    fn a_model_word_becomes_a_chip() {
        assert_eq!(
            take("@opus arregla", false, &[]),
            Some((
                Lead::Chip(Chip::Model {
                    id: "claude-opus-5-5".into(),
                    label: "Opus 5.5".into()
                }),
                "arregla".into()
            ))
        );
        // Not until the word is finished.
        assert_eq!(
            take("@opus", false, &[]),
            Some((Lead::Scope(Scope::People), "opus".into()))
        );
        assert!(matches!(
            take("@hai x", true, &[]),
            Some((Lead::Chip(Chip::Model { .. }), _))
        ));
        assert_eq!(model_of("op"), None);
    }

    #[test]
    fn a_known_branch_becomes_a_chip() {
        assert_eq!(
            take("phone-local arregla el build", false, &branches()),
            Some((
                Lead::Chip(Chip::Branch("phone-local".into())),
                "arregla el build".into()
            ))
        );
        assert_eq!(take("main arregla", false, &branches()), None);
        assert_eq!(take("phone-local", false, &branches()), None);
        assert_eq!(take("unknown-branch x", false, &branches()), None);
    }

    #[test]
    fn questions_are_told_from_searches() {
        assert!(is_question("¿por qué falló el CI de phone-local?"));
        assert!(is_question("why did the relay build fail"));
        assert!(is_question("is the relay down?"));
        assert!(!is_question("sidebar"));
        assert!(!is_question("island card"));
        assert!(!is_question("how"));
    }
}
