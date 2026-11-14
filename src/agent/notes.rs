//! Notes that ride along with the next prompt: annotations on parts of a
//! reply and review comments on lines of a file or diff. They wait above the
//! composer and are written out after the typed text when it is sent.

use std::ops::Range;

use super::attach;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// A remark on something the agent wrote.
    Annotation,
    /// A comment on lines of a file or diff.
    Review,
}

#[derive(Clone, Debug)]
pub struct ChatNote {
    pub kind: NoteKind,
    pub quote: String,
    pub note: String,
    /// The file the quote comes from, as the session would mention it.
    pub path: Option<String>,
    /// First and last line (1-based) of the quote in `path`.
    pub lines: Option<(usize, usize)>,
    /// Where the quote sits in a transcript: session id, surface, range.
    pub mark: Option<(String, usize, Range<usize>)>,
}

impl ChatNote {
    /// The short text on the note's chip.
    pub fn label(&self) -> String {
        let src = if self.note.trim().is_empty() {
            &self.quote
        } else {
            &self.note
        };
        let line = src
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        let mut s: String = line.chars().take(40).collect();
        if line.chars().count() > 40 {
            s.push('…');
        }
        match (&self.path, self.lines) {
            (Some(p), Some(l)) => format!("{}:{} {s}", short_path(p), line_label(l)),
            (Some(p), None) => format!("{} {s}", short_path(p)),
            _ => s,
        }
    }
}

fn short_path(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn line_label((a, b): (usize, usize)) -> String {
    if a == b {
        format!("{a}")
    } else {
        format!("{a}-{b}")
    }
}

fn quoted(text: &str, indent: &str) -> String {
    text.lines()
        .map(|l| format!("{indent}> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn source(n: &ChatNote) -> Option<String> {
    let p = n.path.as_deref()?;
    Some(match n.lines {
        Some((a, b)) if a == b => format!("{} (line {a})", attach::mention(p)),
        Some((a, b)) => format!("{} (lines {a}-{b})", attach::mention(p)),
        None => attach::mention(p),
    })
}

/// The notes as prompt text: annotations as quotes with the remark under
/// each, review comments as a numbered list with `@path (lines)` references.
pub fn format(notes: &[ChatNote]) -> String {
    let mut parts = Vec::new();
    let annotations: Vec<&ChatNote> = notes
        .iter()
        .filter(|n| n.kind == NoteKind::Annotation)
        .collect();
    if !annotations.is_empty() {
        let mut s = String::from(if annotations.len() == 1 {
            "About this part of your reply:"
        } else {
            "About these parts of your reply:"
        });
        for n in annotations {
            s.push_str("\n\n");
            if let Some(src) = source(n) {
                s.push_str(&src);
                s.push('\n');
            }
            s.push_str(&quoted(n.quote.trim_end(), ""));
            if !n.note.trim().is_empty() {
                s.push('\n');
                s.push_str(n.note.trim());
            }
        }
        parts.push(s);
    }
    let reviews: Vec<&ChatNote> = notes
        .iter()
        .filter(|n| n.kind == NoteKind::Review)
        .collect();
    if !reviews.is_empty() {
        let mut s = String::from("Review comments. Address each one:");
        for (i, n) in reviews.iter().enumerate() {
            s.push_str(&format!("\n\n{}. ", i + 1));
            s.push_str(&source(n).unwrap_or_default());
            if !n.quote.trim().is_empty() {
                s.push('\n');
                s.push_str(&quoted(n.quote.trim_end(), "   "));
            }
            s.push_str("\n   ");
            s.push_str(&n.note.trim().replace('\n', "\n   "));
        }
        parts.push(s);
    }
    parts.join("\n\n")
}

/// The messages of a transcript as "User:" / "Agent:" paragraphs, tool
/// calls as one line each.
fn messages(entries: &[super::Entry], tools: bool) -> Vec<String> {
    use super::Entry;
    entries
        .iter()
        .filter_map(|e| match e {
            Entry::User { text, .. } => Some(format!("User: {}", text.trim())),
            Entry::Agent { text } if !text.trim().is_empty() => {
                Some(format!("Agent: {}", text.trim()))
            }
            Entry::Tool(t) if tools => Some(format!("[tool] {}", t.title.trim())),
            _ => None,
        })
        .collect()
}

/// Another session's conversation for a prompt, the newest messages that fit
/// in `budget` bytes.
pub fn context_block(title: &str, agent: &str, entries: &[super::Entry], budget: usize) -> String {
    let all = messages(entries, false);
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for m in all.iter().rev() {
        if used + m.len() > budget {
            if kept.is_empty() {
                let mut end = budget.min(m.len());
                while !m.is_char_boundary(end) {
                    end -= 1;
                }
                kept.push(&m[..end]);
            }
            break;
        }
        used += m.len() + 2;
        kept.push(m);
    }
    kept.reverse();
    let omitted = all.len() - kept.len();
    let title = title.replace('"', "'");
    let mut s = format!("<attached_context session=\"{title}\" agent=\"{agent}\">\n");
    if omitted > 0 {
        s.push_str(&format!("[{omitted} earlier messages left out]\n\n"));
    }
    s.push_str(&kept.join("\n\n"));
    s.push_str("\n</attached_context>");
    s
}

/// A whole transcript as markdown, for a session linked into another.
pub fn transcript_markdown(title: &str, agent: &str, entries: &[super::Entry]) -> String {
    let mut s = format!("# {title}\n\nA {agent} session in Arakun.\n\n");
    s.push_str(&messages(entries, true).join("\n\n"));
    s.push('\n');
    s
}

/// What an agent taking over `from`'s conversation is told with its first
/// prompt: the newest messages that fit in `budget` bytes, and where the whole
/// transcript is.
pub fn handoff(
    from: &str,
    title: &str,
    entries: &[super::Entry],
    transcript: &str,
    budget: usize,
) -> String {
    format!(
        "<handoff from=\"{from}\">\n\
         This conversation started with {from} in Arakun and you are taking it over; \
         the user expects you to know what was said. The latest messages are below, \
         and the whole transcript, with every tool call, is in {}. Changes already in \
         this checkout include {from}'s work.\n\n{}\n</handoff>",
        attach::mention(transcript),
        context_block(title, from, entries, budget)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_messages_in_context() {
        use super::super::Entry;
        let entries = vec![
            Entry::User {
                text: "first question".into(),
                at: 0,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            Entry::Agent {
                text: "a long first answer".into(),
            },
            Entry::User {
                text: "second".into(),
                at: 0,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            Entry::Agent {
                text: "done".into(),
            },
        ];
        let block = context_block("Fix \"auth\"", "Droid", &entries, 30);
        assert_eq!(
            block,
            "<attached_context session=\"Fix 'auth'\" agent=\"Droid\">\n[2 earlier messages left out]\n\nUser: second\n\nAgent: done\n</attached_context>"
        );
        assert!(transcript_markdown("T", "Droid", &entries).contains("Agent: a long first answer"));
    }

    #[test]
    fn hands_the_conversation_over() {
        use super::super::Entry;
        let entries = vec![
            Entry::User {
                text: "fix the login".into(),
                at: 0,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            Entry::Agent {
                text: "fixed".into(),
            },
        ];
        let text = handoff("Droid", "Login", &entries, "/data/linked/s1.md", 1024);
        assert!(text.starts_with(
            "<handoff from=\"Droid\">
This conversation started with Droid in Arakun"
        ));
        assert!(text.contains("is in @/data/linked/s1.md."), "{text}");
        assert!(text.contains("include Droid's work."));
        assert!(text.ends_with(
            "<attached_context session=\"Login\" agent=\"Droid\">
User: fix the login

Agent: fixed
</attached_context>
</handoff>"
        ));
    }

    fn note(
        kind: NoteKind,
        quote: &str,
        note: &str,
        path: Option<&str>,
        lines: Option<(usize, usize)>,
    ) -> ChatNote {
        ChatNote {
            kind,
            quote: quote.into(),
            note: note.into(),
            path: path.map(Into::into),
            lines,
            mark: None,
        }
    }

    #[test]
    fn formats_annotations_and_reviews() {
        let notes = [
            note(
                NoteKind::Annotation,
                "use a mutex\nhere",
                "why not a channel?",
                None,
                None,
            ),
            note(
                NoteKind::Review,
                "let x = 1;",
                "rename x",
                Some("src/a b.rs"),
                Some((12, 12)),
            ),
            note(
                NoteKind::Review,
                "",
                "split this",
                Some("lib.rs"),
                Some((3, 5)),
            ),
        ];
        assert_eq!(
            format(&notes),
            "About this part of your reply:\n\n> use a mutex\n> here\nwhy not a channel?\n\n\
             Review comments. Address each one:\n\n1. @\"src/a b.rs\" (line 12)\n   > let x = 1;\n   rename x\n\n\
             2. @lib.rs (lines 3-5)\n   split this"
        );
        assert_eq!(notes[1].label(), "a b.rs:12 rename x");
        assert_eq!(notes[2].label(), "lib.rs:3-5 split this");
        assert_eq!(notes[0].label(), "why not a channel?");
        assert_eq!(format(&[]), "");
    }
}
