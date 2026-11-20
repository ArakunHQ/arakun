//! Scripts agents keep writing: the shell scripts in a project's saved
//! transcripts, grouped by likeness, for `arakun mem scripts`. Agents write the
//! same helper (run the checks, read a pull request's reviews) again in session
//! after session; the report shows which ones are worth keeping as a script.
//!
//! It only reports. Matching is by the words a script uses, not its exact text,
//! since agents rarely write a script the same way twice.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Scripts at least this much alike (shared words over all words) count as one.
const ALIKE: f32 = 0.6;
/// A one-line command this long, with more than one step, counts as a script.
const LONG_LINE: usize = 120;
/// Fewer words than this is a command, not a script.
const MIN_WORDS: usize = 8;
const SAMPLE_LINES: usize = 6;
const SAMPLE_WIDTH: usize = 100;

/// Commands that only read or look around. A script made of nothing else is the
/// agent exploring the code, which is never the same twice.
const PLAIN: &[&str] = &[
    "sed", "grep", "rg", "cat", "head", "tail", "ls", "find", "wc", "echo", "awk", "sort", "uniq",
    "cut", "tr", "nl", "printf", "true", "false", "test", "[", "[[", "diff", "git", "xargs", "cd",
    "pwd", "sleep", "export", "set", "for", "while", "if", "do", "done", "then", "fi", "else",
    "elif", "{", "}", "(", ")", "!", "time",
];

/// What edits a file in place: a one-off change, not a helper to keep.
const EDITS: &[&str] = &[
    ".write(",
    "write_text(",
    "sed -i",
    "perl -pi",
    "perl -i",
    "cat >",
    "tee ",
];

#[derive(Deserialize)]
struct File {
    meta: Meta,
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(default)]
    cwd: PathBuf,
    #[serde(default)]
    updated: u64,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    raw_input: Option<serde_json::Value>,
}

/// One script an agent ran, from the session `session` last active at `at`.
#[derive(Clone, Debug)]
pub struct Script {
    pub text: String,
    pub session: String,
    pub at: u64,
}

/// The scripts in the transcripts in `dir` of the sessions `belongs` accepts
/// (by their working directory) that were active since `since`, oldest first,
/// and how many sessions were read.
pub fn scripts_in(
    dir: &Path,
    since: u64,
    mut belongs: impl FnMut(&Path) -> bool,
) -> (Vec<Script>, usize) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return (Vec::new(), 0);
    };
    let (mut out, mut sessions) = (Vec::new(), 0);
    for e in read.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        // Not touched since: skip it without reading.
        let modified = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        if modified.is_some_and(|m| m < since) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(file) = serde_json::from_slice::<File>(&bytes) else {
            continue;
        };
        if file.meta.updated < since || !belongs(&file.meta.cwd) {
            continue;
        }
        sessions += 1;
        let session = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        for entry in &file.entries {
            if entry.kind != "tool" {
                continue;
            }
            if let Some(text) = entry.raw_input.as_ref().and_then(command).and_then(script) {
                out.push(Script {
                    text,
                    session: session.clone(),
                    at: file.meta.updated,
                });
            }
        }
    }
    out.sort_by_key(|s| s.at);
    (out, sessions)
}

/// The command line of a shell tool call.
fn command(input: &serde_json::Value) -> Option<String> {
    match input.get("command").or_else(|| input.get("cmd"))? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// `cmd` without a leading `cd <dir>`: agents start with one to reach their
/// worktree, which differs every session.
fn without_cd(cmd: &str) -> &str {
    let t = cmd.trim();
    let Some(rest) = t.strip_prefix("cd ") else {
        return t;
    };
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ';' || c == '&')
        .unwrap_or(rest.len());
    let after = rest[end..].trim_start_matches([' ', '\t']);
    for sep in ["&&", ";", "\n"] {
        if let Some(r) = after.strip_prefix(sep) {
            return r.trim_start();
        }
    }
    if after.is_empty() { "" } else { t }
}

/// `cmd` with each quoted string (on one line) replaced by `"`: what is quoted
/// is data, which changes from one run to the next.
fn unquoted(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' && c != '\'' {
            out.push(c);
            continue;
        }
        let mut closed = false;
        let mut inner = String::new();
        while let Some(&n) = chars.peek() {
            if n == '\n' {
                break;
            }
            chars.next();
            if c == '"' && n == '\\' {
                if let Some(esc) = chars.next() {
                    inner.push(n);
                    inner.push(esc);
                }
                continue;
            }
            if n == c {
                closed = true;
                break;
            }
            inner.push(n);
        }
        if closed {
            out.push('"');
        } else {
            // An apostrophe in a heredoc's text, not a quote.
            out.push(c);
            out.push_str(&inner);
        }
    }
    out
}

/// The script in `cmd`, or `None` when it is a short command, an edit, or only
/// reads.
fn script(cmd: String) -> Option<String> {
    let text = without_cd(&cmd);
    if EDITS.iter().any(|e| text.contains(e)) {
        return None;
    }
    let bare = unquoted(text);
    let steps: Vec<&str> = bare
        .split(['\n', ';', '|'])
        .flat_map(|s| s.split("&&"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let lines = text.lines().count();
    if lines < 3 && !(text.len() >= LONG_LINE && steps.len() > 1) {
        return None;
    }
    let only_reads = steps.iter().all(|s| {
        let program = s
            .split_whitespace()
            .find(|w| !w.contains('=') || w.starts_with('='))
            .unwrap_or("")
            .trim_start_matches(['(', '{', '!']);
        program.is_empty() || PLAIN.contains(&program)
    });
    if only_reads || words(text).len() < MIN_WORDS {
        return None;
    }
    Some(text.to_string())
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

/// The words of a script, hashed and sorted: names, flags and the operators
/// between steps, leaving out quoted strings and numbers.
fn words(text: &str) -> Vec<u64> {
    let bare = unquoted(text);
    let mut out: Vec<u64> = Vec::new();
    let mut word = String::new();
    fn flush(w: &mut String, out: &mut Vec<u64>) {
        if w.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '-')
            || w.starts_with(['|', ';', '&', '<', '>'])
        {
            out.push(fnv(w));
        }
        w.clear();
    }
    let op = |c: char| matches!(c, '|' | ';' | '&' | '<' | '>');
    for c in bare.chars() {
        let name = c.is_alphanumeric() || matches!(c, '_' | '.' | '-');
        let same = if word.starts_with(op) { op(c) } else { name };
        if !same && !word.is_empty() {
            flush(&mut word, &mut out);
        }
        if name || op(c) {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

/// Shared words over all words of two sorted lists.
fn alike(a: &[u64], b: &[u64]) -> f32 {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if long.is_empty() || (short.len() as f32) < ALIKE * long.len() as f32 {
        return 0.;
    }
    let (mut i, mut j, mut both) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    both as f32 / (a.len() + b.len() - both) as f32
}

/// Scripts alike enough to be one: how often it was written, in how many
/// sessions, when last, and its latest version.
#[derive(Debug)]
pub struct Repeat {
    pub times: usize,
    pub sessions: usize,
    pub last: u64,
    pub sample: String,
}

/// Groups `scripts` (oldest first) by likeness, most sessions first. Each group
/// is matched against its first script, so it doesn't drift.
pub fn repeats(scripts: &[Script]) -> Vec<Repeat> {
    struct Group<'a> {
        seed: Vec<u64>,
        times: usize,
        sessions: HashSet<&'a str>,
        last: &'a Script,
    }
    let mut groups: Vec<Group> = Vec::new();
    for s in scripts {
        let w = words(&s.text);
        match groups.iter_mut().find(|g| alike(&g.seed, &w) >= ALIKE) {
            Some(g) => {
                g.times += 1;
                g.sessions.insert(&s.session);
                g.last = s;
            }
            None => groups.push(Group {
                seed: w,
                times: 1,
                sessions: HashSet::from([s.session.as_str()]),
                last: s,
            }),
        }
    }
    let mut out: Vec<Repeat> = groups
        .into_iter()
        .map(|g| Repeat {
            times: g.times,
            sessions: g.sessions.len(),
            last: g.last.at,
            sample: g.last.text.clone(),
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse((r.sessions, r.times, r.last)));
    out
}

/// The first lines of a script, cut to fit, indented for the report.
fn sample(text: &str) -> String {
    // A one-line script reads better a step per line, like a written one.
    let text = if text.contains('\n') {
        text.to_string()
    } else {
        text.replace(" && ", " &&\n").replace("; ", ";\n")
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(SAMPLE_LINES)
        .map(|l| {
            let l = l.trim_end();
            if l.chars().count() > SAMPLE_WIDTH {
                let cut: String = l.chars().take(SAMPLE_WIDTH - 1).collect();
                format!("   {cut}…")
            } else {
                format!("   {l}")
            }
        })
        .collect();
    if lines.len() > SAMPLE_LINES {
        out.push(format!("   …{} more lines", lines.len() - SAMPLE_LINES));
    }
    out.join("\n")
}

pub const USAGE: &str = "\
arakun mem scripts [--days N] [--min N] [-n N]
    scripts agents wrote again in this project's sessions: seen in at least
    --min sessions (3) of the last --days (30), the -n (5) most frequent";

/// `arakun mem scripts`: the report for the project at `project`, from the
/// transcripts in `dir`.
pub fn run(args: &[String], project: &Path, dir: &Path, at: u64) -> Result<String, String> {
    let (mut days, mut min, mut limit) = (30u64, 3usize, 5usize);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = || {
            it.next()
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("usage: {a} needs a number above 0\n{USAGE}"))
        };
        match a.as_str() {
            "--days" => days = num()? as u64,
            "--min" => min = num()?,
            "-n" => limit = num()?,
            _ => return Err(format!("usage: unknown `{a}`\n{USAGE}")),
        }
    }
    let name = project.file_name().map_or_else(
        || project.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    // Worktrees arakun made and has since removed no longer say which project
    // they were of; their folder under `~/arakun-workspaces/<project>` does.
    let workspaces = crate::model::workspaces_root().join(&name);
    let mut known: HashMap<PathBuf, bool> = HashMap::new();
    let belongs = |cwd: &Path| {
        if cwd.starts_with(project) || cwd.starts_with(&workspaces) {
            return true;
        }
        *known
            .entry(cwd.to_path_buf())
            .or_insert_with(|| cwd.is_dir() && crate::memory::project_of(cwd) == project)
    };
    let since = at.saturating_sub(days * 86400);
    let (scripts, sessions) = scripts_in(dir, since, belongs);
    let found: Vec<Repeat> = repeats(&scripts)
        .into_iter()
        .filter(|r| r.sessions >= min)
        .collect();
    let span = if days == 1 {
        "the last day".to_string()
    } else {
        format!("the last {days} days")
    };
    if found.is_empty() {
        return Ok(format!(
            "No script was written again in {min} or more of {name}'s {sessions} sessions in {span}."
        ));
    }
    let mut out = vec![format!(
        "Scripts agents wrote again in {name}'s {sessions} sessions in {span}:"
    )];
    for (i, r) in found.iter().take(limit).enumerate() {
        out.push(String::new());
        out.push(format!(
            "{}. written {} times in {} sessions, last {}",
            i + 1,
            r.times,
            r.sessions,
            crate::memory::ago(r.last, at)
        ));
        out.push(sample(&r.sample));
    }
    if found.len() > limit {
        out.push(String::new());
        out.push(format!(
            "…{} more (-n {})",
            found.len() - limit,
            found.len()
        ));
    }
    out.push(String::new());
    out.push(
        "Keep one as a script in the project (tools/, say), or save it with \
         `arakun mem save \"<title>\" --kind pattern --body -`, so later sessions run it \
         instead of writing it again."
            .into(),
    );
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arakun-repeats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn transcript(dir: &Path, id: &str, cwd: &str, updated: u64, commands: &[&str]) {
        let entries: Vec<serde_json::Value> = commands
            .iter()
            .map(|c| {
                serde_json::json!({
                    "type": "tool",
                    "id": "t",
                    "title": "Bash",
                    "kind": "execute",
                    "status": "completed",
                    "raw_input": { "command": c },
                })
            })
            .chain([serde_json::json!({ "type": "agent", "text": "done" })])
            .collect();
        let t = serde_json::json!({
            "version": 1,
            "meta": { "agent": "claude", "cwd": cwd, "created": updated, "updated": updated },
            "entries": entries,
        });
        std::fs::write(dir.join(format!("{id}.json")), t.to_string()).unwrap();
    }

    const CHECKS: &str = "tools/rustfmt.sh --check && cargo clippy --locked --all-targets \
        --no-default-features -- -D warnings 2>&1 | tail -20 && cargo test --locked \
        --no-default-features git 2>&1 | grep -E \"test result|FAILED\"";

    #[test]
    fn a_script_counts_without_its_cd_and_its_data() {
        assert_eq!(without_cd("cd /a/b && make all"), "make all");
        assert_eq!(without_cd("cd /a/b; make all"), "make all");
        assert_eq!(without_cd("cd /a/b\nmake all"), "make all");
        assert_eq!(without_cd("cd /a/b || exit"), "cd /a/b || exit");
        let a = words(&CHECKS.replace(" git ", " memory "));
        let b = words(&format!("cd /x/env-a && {CHECKS}"));
        assert!(alike(&a, &b) >= ALIKE, "{}", alike(&a, &b));
        assert_eq!(unquoted("echo \"a|b\" 'c;d' it's"), "echo \" \" it's");
    }

    #[test]
    fn edits_reads_and_short_commands_are_no_scripts() {
        assert!(script(CHECKS.into()).is_some());
        let heredoc = "python3 - <<'EOF'\nimport json, sys\nd = json.load(open('big.json'))\n\
            for k, v in d.items():\n    print(k, type(v).__name__, len(str(v)))\nEOF";
        assert!(script(heredoc.into()).is_some());
        // A file edit through Python is a one-off.
        let edit = "python3 - <<'EOF'\np='src/a.rs'; s=open(p).read()\n\
            s=s.replace('a', 'b')\nopen(p,'w').write(s)\nEOF";
        assert!(script(edit.into()).is_none());
        // Looking around the code.
        let look = "sed -n 1,100p src/shell/new_dialog.rs; grep -n \"pub path\\|pub struct\" \
            -A12 src/model.rs | head -30; grep -rn foo src | head";
        assert!(script(look.into()).is_none());
        assert!(script("cargo test --locked memory".into()).is_none());
    }

    #[test]
    fn reports_scripts_written_again_in_other_sessions() {
        let dir = tmp("report");
        let project = Path::new("/work/demo");
        let reviews = "gh pr view 12 --json commits --jq '.commits[]|.oid'; gh api \
            repos/o/r/pulls/12/reviews --jq '.[]|{id,user:.user.login,state}'; gh api \
            repos/o/r/pulls/12/comments --jq '.[]|{id,path,line,body}'";
        for (i, cwd) in ["/work/demo", "/work/demo/sub", "/work/demo"]
            .iter()
            .enumerate()
        {
            let checks = CHECKS.replace(" git ", [" git ", " memory ", " state "][i]);
            transcript(
                &dir,
                &format!("s{i}"),
                cwd,
                1000 + i as u64,
                &[&checks, "ls"],
            );
        }
        transcript(
            &dir,
            "s3",
            "/work/demo",
            1003,
            &[&reviews.replace("12", "13")],
        );
        transcript(&dir, "s4", "/work/demo", 1004, &[reviews, reviews]);
        // Another project's sessions don't count.
        for i in 5..8 {
            transcript(&dir, &format!("s{i}"), "/work/other", 1000, &[reviews]);
        }
        let at = 2000;
        let r = run(&[], project, &dir, at).unwrap();
        assert!(r.contains("demo's 5 sessions"), "{r}");
        assert!(r.contains("1. written 3 times in 3 sessions"), "{r}");
        assert!(
            r.contains("   tools/rustfmt.sh --check &&\n   cargo clippy"),
            "{r}"
        );
        assert!(!r.contains("2. "), "{r}");
        let r = run(&s(&["--min", "2"]), project, &dir, at).unwrap();
        assert!(r.contains("2. written 3 times in 2 sessions"), "{r}");
        // Sessions older than --days are left out.
        let r = run(&s(&["--days", "1"]), project, &dir, 1000 + 86400 + 2).unwrap();
        assert!(r.starts_with("No script was written again"), "{r}");
        assert!(run(&s(&["--min"]), project, &dir, at).is_err());
        assert!(run(&s(&["--bogus"]), project, &dir, at).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_long_sample_is_cut() {
        let text = (0..10)
            .map(|i| format!("line{i} {}", "x".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        let s = sample(&text);
        assert_eq!(s.lines().count(), SAMPLE_LINES + 1);
        assert!(s.ends_with("…4 more lines"));
        assert!(s.lines().all(|l| l.chars().count() <= SAMPLE_WIDTH + 3));
        assert_eq!(
            sample("make a && make b; make c"),
            "   make a &&\n   make b;\n   make c"
        );
    }
}
