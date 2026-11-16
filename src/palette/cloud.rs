//! "Mandar al agente de la nube…": work for arakun-pilot typed in the
//! palette ("nube: arregla el título de la pestaña"). It goes to the user's
//! open pull request picked in the GitHub tab, or to a new branch named
//! after the text. The request itself lives in `crate::github::pilot`.

use crate::github::pilot::{self, TaskRequest, TaskTarget};

/// The words that start a task, lower case, longest first so "agente nube:"
/// isn't read as "nube:" with "agente" in front.
const LEADS: &[&str] = &[
    "agente de la nube:",
    "agente nube:",
    "mandar a la nube:",
    "nube:",
];

/// The longest branch name suggested; the user can still type a longer one.
const SLUG_MAX: usize = 40;

/// What the palette knows of arakun-pilot when it opens: there only when
/// its `client.json` exists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PilotSeed {
    /// The user's open pull request picked in the GitHub tab: (number, title).
    pub pull: Option<(u64, String)>,
    /// Whether `client.json` has the `admin_token` tasks need.
    pub admin: bool,
}

/// The task in `q` ("nube: <task>"), possibly empty: the card shows from the
/// lead on, so it can say what to type.
pub fn parse(q: &str) -> Option<&str> {
    let t = q.trim_start();
    LEADS.iter().find_map(|lead| {
        let head = t.get(..lead.len())?;
        head.eq_ignore_ascii_case(lead)
            .then(|| t[lead.len()..].trim())
    })
}

/// The task the card is for: only when arakun-pilot is set up.
pub fn card_task<'a>(q: &'a str, seed: Option<&PilotSeed>) -> Option<&'a str> {
    seed.and(parse(q))
}

/// A short title for the task: its first line, cut at a word near 72
/// characters.
pub fn title(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    if line.chars().count() <= 72 {
        return line.to_string();
    }
    let cut: String = line.chars().take(71).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > 30 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

/// `text` cut to `max` characters, with "…" when it was longer.
pub fn short(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A branch name from the task: `pilot/` and its first words, without
/// accents, in kebab case.
pub fn suggest_branch(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut out = String::new();
    for word in slug.split('-').filter(|w| !w.is_empty()) {
        if !out.is_empty() && out.len() + 1 + word.len() > SLUG_MAX {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(word);
    }
    out.truncate(SLUG_MAX);
    if out.is_empty() {
        "pilot/tarea".into()
    } else {
        format!("pilot/{out}")
    }
}

/// Why git would refuse `name` as a new branch, in Spanish; None when it
/// takes it. The common cases of `git check-ref-format`, enough to catch a
/// typo before it reaches the Worker.
pub fn branch_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Ponle nombre a la rama.");
    }
    let bad_char = name
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    let bad = bad_char
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name == "@"
        || name.starts_with(['-', '/', '.'])
        || name.ends_with(['/', '.'])
        || name.ends_with(".lock")
        || name.split('/').any(|p| p.starts_with('.'));
    if bad {
        return Some("Ese nombre de rama no vale: usa letras, números, - y /.");
    }
    (name == "main").then_some("La tarea no puede ir directo a main.")
}

/// The request for `task`, or why it can't go yet. A task goes to the
/// picked pull request when `to_pull` and there is one, else to `branch`
/// off main.
pub fn request(
    task: &str,
    seed: &PilotSeed,
    to_pull: bool,
    branch: &str,
) -> Result<TaskRequest, String> {
    let prompt = task.trim();
    if prompt.is_empty() {
        return Err("Escribe qué debe hacer el agente después de “nube:”.".into());
    }
    if !seed.admin {
        return Err(pilot::NO_ADMIN.into());
    }
    let target = match seed.pull.as_ref().filter(|_| to_pull) {
        Some((n, _)) => TaskTarget::Pull(*n),
        None => {
            let name = branch.trim();
            if let Some(why) = branch_problem(name) {
                return Err(why.into());
            }
            TaskTarget::Branch {
                name: name.to_string(),
                base: "main".into(),
            }
        }
    };
    Ok(TaskRequest {
        title: title(prompt),
        prompt: prompt.to_string(),
        target,
    })
}

/// The footer's line once the Worker took a task.
pub fn sent_note(id: &str) -> String {
    let id = id.trim();
    if id.is_empty() {
        "Mandado al agente de la nube. El resultado llega como tarjeta en GitHub.".into()
    } else {
        format!("Mandado al agente de la nube ({id}). El resultado llega como tarjeta en GitHub.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_starts_with_its_lead() {
        assert_eq!(parse("nube: arregla el título"), Some("arregla el título"));
        assert_eq!(parse("Nube:arregla"), Some("arregla"));
        assert_eq!(
            parse("agente nube: sube la versión"),
            Some("sube la versión")
        );
        assert_eq!(parse("Agente de la nube: algo"), Some("algo"));
        // The lead alone shows the card, which says what to type.
        assert_eq!(parse("nube:"), Some(""));
        assert_eq!(parse("nube"), None);
        assert_eq!(parse("la nube: algo"), None);
        assert_eq!(parse("¿qué es la nube?"), None);
    }

    #[test]
    fn the_card_shows_only_with_pilot_set_up() {
        let seed = PilotSeed::default();
        assert_eq!(card_task("nube: x", Some(&seed)), Some("x"));
        assert_eq!(card_task("nube: x", None), None);
        assert_eq!(card_task("arregla x", Some(&seed)), None);
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(title("  arregla el tab\nmás detalle"), "arregla el tab");
        let long = "haz que la paleta recuerde lo que abriste a esta hora y lo ponga arriba de todo lo demás";
        let t = title(long);
        assert!(t.ends_with('…') && t.chars().count() <= 72, "{t}");
        assert!(long.starts_with(t.trim_end_matches('…')));
        assert_eq!(short("feat(github): add a tab", 12), "feat(github…");
        assert_eq!(short(" corto ", 12), "corto");
    }

    #[test]
    fn branches_are_named_after_the_task() {
        assert_eq!(
            suggest_branch("Arregla el título de la pestaña"),
            "pilot/arregla-el-titulo-de-la-pestana"
        );
        assert_eq!(suggest_branch("¿¡ !?"), "pilot/tarea");
        let long = suggest_branch(
            "agrega una opción para que las notificaciones de github suenen distinto",
        );
        assert!(long.len() <= "pilot/".len() + SLUG_MAX, "{long}");
        assert!(!long.ends_with('-'));
        assert_eq!(branch_problem(&long), None);
    }

    #[test]
    fn the_request_goes_to_the_pull_or_a_new_branch() {
        let seed = PilotSeed {
            pull: Some((112, "feat: x".into())),
            admin: true,
        };
        let r = request("  arregla el tab  ", &seed, true, "pilot/arregla-el-tab").unwrap();
        assert_eq!(r.target, TaskTarget::Pull(112));
        assert_eq!(
            (r.title.as_str(), r.prompt.as_str()),
            ("arregla el tab", "arregla el tab")
        );
        // Switched to a new branch, or no pull request picked.
        let r = request("arregla", &seed, false, " pilot/arregla ").unwrap();
        assert_eq!(
            r.target,
            TaskTarget::Branch {
                name: "pilot/arregla".into(),
                base: "main".into()
            }
        );
        let alone = PilotSeed {
            pull: None,
            admin: true,
        };
        assert!(matches!(
            request("arregla", &alone, true, "pilot/a").unwrap().target,
            TaskTarget::Branch { .. }
        ));
        // What stops it.
        assert!(request("  ", &seed, true, "x").is_err());
        assert!(request("arregla", &alone, true, "mal nombre").is_err());
        let no_admin = PilotSeed {
            admin: false,
            ..seed
        };
        assert_eq!(
            request("arregla", &no_admin, true, "x").unwrap_err(),
            pilot::NO_ADMIN
        );
        assert!(sent_note("t_42").contains("(t_42)"));
        assert!(!sent_note("").contains("()"));
    }

    #[test]
    fn bad_branch_names_are_caught() {
        assert_eq!(branch_problem("pilot/fix-tab"), None);
        assert_eq!(branch_problem("fix/a.b"), None);
        for bad in [
            "",
            "con espacio",
            "a..b",
            "-x",
            "x/",
            "x.lock",
            "a//b",
            "x:y",
            "a/.b",
            "main",
        ] {
            assert!(branch_problem(bad).is_some(), "{bad}");
        }
    }
}
