//! The `arakun …` commands agents are told about (`HINT` in `launch.rs` and the
//! memory protocol), read as what they do: "Search memory", "Screenshot the
//! page", "New agent". The row shows that instead of the command line; opening
//! it still shows the command and its output.

/// What an `arakun` command does, for its row in the transcript.
#[derive(Debug, PartialEq)]
pub(super) struct ArakunCall {
    pub(super) action: &'static str,
    pub(super) target: String,
    pub(super) icon: &'static str,
    /// The target is a command, path, address or selector, not words.
    pub(super) mono: bool,
    /// `arakun shrink -- <cmd>`: the target is a shell command, so the row
    /// says "Ran" like any other.
    pub(super) shell: bool,
}

/// The `arakun` command in `command`, read as what it does. None for anything
/// else, and for a line that also runs other commands, so a row never hides
/// part of what ran.
pub(super) fn parse(command: &str) -> Option<ArakunCall> {
    let words = arakun_words(command)?;
    let (sub, rest) = words.split_first()?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let call = |action, target: String, icon, mono| ArakunCall {
        action,
        target,
        icon,
        mono,
        shell: false,
    };
    Some(match sub.as_str() {
        "mem" => memory(&rest)?,
        "browse" => call("Browse", positional(&rest, &[]).join(" "), "globe", true),
        "open" => call("Open", positional(&rest, &[]).join(" "), "file01", true),
        "run" => call(
            "Start",
            positional_until_command(&rest, &["--port", "--ready", "--timeout"]).join(" "),
            "play",
            true,
        ),
        "logs" => call(
            "Read logs",
            positional(&rest, &["--until", "--timeout", "-n"]).join(" "),
            "computer-terminal01",
            true,
        ),
        "page" => page(&rest)?,
        "check" => call("Check the project", String::new(), "check-list", false),
        "shrink" => {
            let inner = match rest.iter().position(|w| *w == "--") {
                Some(i) => &rest[i + 1..],
                None => &rest[..],
            };
            if inner.is_empty() {
                return None;
            }
            ArakunCall {
                action: "",
                target: inner.join(" "),
                icon: "",
                mono: true,
                shell: true,
            }
        }
        "fetch" => call("Fetch", positional(&rest, &[]).join(" "), "globe", true),
        "crawl" => call(
            "Crawl",
            positional(&rest, &["--limit", "--depth", "--include", "--exclude"]).join(" "),
            "globe",
            true,
        ),
        "agent" => agent(&rest)?,
        "rename" => call("Rename to", rest.join(" "), "git-branch", true),
        "notify" => call("Notify", rest.join(" "), "message01", false),
        "feedback" => call(
            "Send feedback",
            positional(&rest, &["--kind"]).join(" "),
            "comment01",
            false,
        ),
        _ => return None,
    })
}

fn memory(rest: &[&str]) -> Option<ArakunCall> {
    let (sub, rest) = rest.split_first()?;
    let (action, target) = match *sub {
        "search" => ("Search memory", positional(rest, &["-n"]).join(" ")),
        // The title is the first word; the flags carry the text.
        "save" => (
            "Remember",
            positional(rest, &["--kind", "--body", "--files", "--topic"])
                .first()
                .map(|t| t.to_string())
                .unwrap_or_default(),
        ),
        "get" => ("Recall memory", rest.join(", ")),
        "forget" => ("Forget memory", rest.join(", ")),
        "context" | "list" | "prompt" => ("Read memory", String::new()),
        _ => return None,
    };
    Some(ArakunCall {
        action,
        target,
        icon: "ai-idea",
        mono: matches!(*sub, "get" | "forget"),
        shell: false,
    })
}

fn page(rest: &[&str]) -> Option<ArakunCall> {
    let rest = match rest {
        ["-t" | "--tab", _, rest @ ..] => rest,
        _ => rest,
    };
    let (action, target, icon, mono) = match rest {
        [] => ("List the browser tabs", String::new(), "app-window", false),
        ["screenshot", ..] => ("Screenshot the page", String::new(), "image01", false),
        ["console", ..] => (
            "Read the page console",
            String::new(),
            "computer-terminal01",
            false,
        ),
        ["text", ..] => ("Read the page", String::new(), "app-window", false),
        ["open", url, ..] => ("Open page", url.to_string(), "globe", true),
        ["reload", ..] => ("Reload the page", String::new(), "refresh01", false),
        ["click", what @ ..] => ("Click", what.join(" "), "cursor01", true),
        ["type", field, text @ ..] => (
            "Type",
            format!("{} into {field}", text.join(" ")),
            "keyboard",
            true,
        ),
        ["press", key, ..] => ("Press", key.to_string(), "keyboard", true),
        ["eval", js @ ..] => ("Run in the page", js.join(" "), "file-code", true),
        _ => return None,
    };
    Some(ArakunCall {
        action,
        target,
        icon,
        mono,
        shell: false,
    })
}

fn agent(rest: &[&str]) -> Option<ArakunCall> {
    let (sub, rest) = rest.split_first()?;
    let flags = [
        "--model", "--effort", "--dir", "--title", "--policy", "--resume",
    ];
    let (action, target, mono) = match *sub {
        "new" | "run" => {
            // The title says it shorter than the prompt.
            let title = rest
                .iter()
                .position(|w| *w == "--title")
                .and_then(|i| rest.get(i + 1));
            let target = match title {
                Some(t) => t.to_string(),
                None => positional_until_command(rest, &flags).join(" "),
            };
            let action = if *sub == "new" {
                "New agent"
            } else {
                "Run agent"
            };
            (action, target, false)
        }
        "wait" => (
            "Wait for agent",
            positional(rest, &["--timeout"]).join(" "),
            true,
        ),
        "log" => ("Read agent log", positional(rest, &["-n"]).join(" "), true),
        "send" => (
            "Message agent",
            rest.get(1..).map(|t| t.join(" ")).unwrap_or_default(),
            false,
        ),
        "stop" => ("Stop agent", rest.join(" "), true),
        "ls" => ("List agents", String::new(), false),
        _ => return None,
    };
    Some(ArakunCall {
        action,
        target,
        icon: "bot",
        mono,
        shell: false,
    })
}

/// The words that are not flags, skipping the value after each flag in
/// `valued`.
fn positional<'a>(words: &[&'a str], valued: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if valued.contains(w) {
            i += 2;
            continue;
        }
        if !w.starts_with('-') || *w == "-" {
            out.push(*w);
        }
        i += 1;
    }
    out
}

/// Flags come first and the rest is a command or prompt, which may have flags
/// of its own: `arakun run --port 3000 npm run dev -- --host`.
fn positional_until_command<'a, 'w>(words: &'w [&'a str], valued: &[&str]) -> &'w [&'a str] {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if *w == "--" {
            return &words[i + 1..];
        }
        if valued.contains(w) {
            i += 2;
        } else if w.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    words.get(i..).unwrap_or_default()
}

/// The words after `arakun` when `command` runs nothing but that, maybe
/// after `cd dir &&` and with `2>&1`. A pipe, any other redirect or a
/// `VAR=value` changes what the line does, so such a line keeps showing the
/// command as typed.
fn arakun_words(command: &str) -> Option<Vec<String>> {
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    for t in tokens(command)? {
        match t {
            Token::Op(op) if op == "|" || op == "&" => return None,
            Token::Op(_) => segments.push(Vec::new()),
            Token::Word(w) => segments.last_mut()?.push(w),
        }
    }
    segments.retain(|s| !s.is_empty());
    let mut segments = segments
        .into_iter()
        .skip_while(|s| s.first().is_some_and(|w| w == "cd"));
    let words = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    let (bin, rest) = words.split_first()?;
    let name = bin.rsplit(['/', '\\']).next()?;
    if name != "arakun" && name != "arakun.exe" {
        return None;
    }
    Some(rest.to_vec())
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    /// `&&`, `||`, `;`, `|`, `&` or a line break.
    Op(String),
}

/// Splits a shell line the way sh would for plain commands: quotes, escapes
/// and the operators between commands, dropping `2>&1`. None for what it
/// cannot read or that does more, such as an open quote, `$(…)` or another
/// redirect, which leaves the row as it was.
fn tokens(line: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    let end_word = |word: &mut String, in_word: &mut bool, out: &mut Vec<Token>| {
        if *in_word {
            out.push(Token::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            '\n' => {}
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        '$' | '`' => return None,
                        c => word.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(c) => {
                    in_word = true;
                    word.push(c);
                }
                None => return None,
            },
            '$' | '`' | '(' | ')' => return None,
            '#' if !in_word => break,
            c if c == ' ' || c == '\t' => end_word(&mut word, &mut in_word, &mut out),
            '\n' | ';' | '&' | '|' => {
                end_word(&mut word, &mut in_word, &mut out);
                let mut op = c.to_string();
                if c != '\n' && c != ';' && chars.peek() == Some(&c) {
                    chars.next();
                    op.push(c);
                }
                out.push(Token::Op(op));
            }
            // Only `2>&1`, which keeps the output where it was going.
            '>' if word == "2" && in_word => {
                let rest: String = chars.clone().take(2).collect();
                if rest != "&1" {
                    return None;
                }
                chars.nth(1);
                word.clear();
                in_word = false;
            }
            '>' | '<' => return None,
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut word, &mut in_word, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(cmd: &str) -> (&'static str, String) {
        let c = parse(cmd).unwrap_or_else(|| panic!("not read: {cmd}"));
        (c.action, c.target)
    }

    #[test]
    fn memory_commands_read_as_memory() {
        assert_eq!(
            read("arakun mem search tool rows -n 5"),
            ("Search memory", "tool rows".into())
        );
        assert_eq!(
            read(
                r#"arakun mem save "Shrink before tail" --kind gotcha --body "Use arakun shrink.""#
            ),
            ("Remember", "Shrink before tail".into())
        );
        assert_eq!(
            read("arakun mem get 12 g3"),
            ("Recall memory", "12, g3".into())
        );
        assert_eq!(read("arakun mem forget 4"), ("Forget memory", "4".into()));
    }

    #[test]
    fn page_commands_say_what_they_do_to_the_page() {
        assert_eq!(
            read("arakun page screenshot"),
            ("Screenshot the page", String::new())
        );
        assert_eq!(
            read("arakun page -t 2 console"),
            ("Read the page console", String::new())
        );
        assert_eq!(
            read("arakun page open http://localhost:3000"),
            ("Open page", "http://localhost:3000".into())
        );
        assert_eq!(
            read("arakun page click 'Sign in'"),
            ("Click", "Sign in".into())
        );
        assert_eq!(
            read(r##"arakun page type "#email" "a@b.c""##),
            ("Type", "a@b.c into #email".into())
        );
        assert_eq!(
            read("arakun page"),
            ("List the browser tabs", String::new())
        );
    }

    #[test]
    fn the_command_of_run_and_shrink_is_kept_whole() {
        assert_eq!(
            read("arakun run --port 3000 --ready 'Local:' npm run dev -- --host"),
            ("Start", "npm run dev -- --host".into())
        );
        let c = parse("arakun shrink -- cargo test --locked agent").unwrap();
        assert!(c.shell);
        assert_eq!(c.target, "cargo test --locked agent");
    }

    #[test]
    fn an_agent_is_named_by_its_title_or_prompt() {
        assert_eq!(
            read("arakun agent new --model claude --title 'Fix the build' look at CI"),
            ("New agent", "Fix the build".into())
        );
        assert_eq!(
            read("arakun agent new --effort high look at --why CI"),
            ("New agent", "look at --why CI".into())
        );
        assert_eq!(
            read("arakun agent wait --timeout 60 a1"),
            ("Wait for agent", "a1".into())
        );
        assert_eq!(
            read("arakun agent send a1 also run the tests"),
            ("Message agent", "also run the tests".into())
        );
    }

    #[test]
    fn other_commands() {
        assert_eq!(
            read("arakun open -s src/main.rs:12"),
            ("Open", "src/main.rs:12".into())
        );
        assert_eq!(
            read("arakun browse -s https://arakun.dev"),
            ("Browse", "https://arakun.dev".into())
        );
        assert_eq!(read("arakun check"), ("Check the project", String::new()));
        assert_eq!(
            read("arakun logs dev --until ready"),
            ("Read logs", "dev".into())
        );
        assert_eq!(
            read("arakun rename fix/tool-rows"),
            ("Rename to", "fix/tool-rows".into())
        );
        assert_eq!(
            read("arakun feedback --kind ux 'The rows are nice'"),
            ("Send feedback", "The rows are nice".into())
        );
    }

    #[test]
    fn the_command_may_be_wrapped() {
        assert_eq!(
            read("cd web && arakun page reload"),
            ("Reload the page", String::new())
        );
        assert_eq!(
            read("./target/debug/arakun check 2>&1"),
            ("Check the project", String::new())
        );
    }

    #[test]
    fn anything_else_stays_a_command() {
        for cmd in [
            "cargo test",
            "arakun dev start",
            "arakun",
            "cargo build && arakun check",
            "arakun check; rm -rf target",
            "arakun notify \"$(git log -1)\"",
            "arakun page click 'open quote",
            "echo arakun mem search x",
            "arakunx check",
            // These do more than the arakun command, so the row shows them.
            "arakun page text | sh",
            "arakun page text > ~/.bashrc",
            "arakun check 2>/dev/null",
            "arakun check & rm -rf target",
            "LD_PRELOAD=x.so arakun check",
        ] {
            assert_eq!(parse(cmd), None, "{cmd}");
        }
    }
}
