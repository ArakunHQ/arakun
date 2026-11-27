//! The usage log: one line per settled turn in `<app dir>/usage.jsonl`, with
//! what the turn used (tokens, cost, credits). It outlives the sessions, so
//! totals by day, project and model can be worked out later.
//!
//! Turns of arakun's own Workers AI loop also say how much each tool's
//! results put in the context (`tools`), which `arakun usage tools` adds up:
//! where to aim output shrinking, and which commands are the noisy ones.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::AgentKind;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// Unix seconds when the turn settled.
    pub at: u64,
    pub agent: AgentKind,
    /// The agent's session id.
    pub session: String,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// USD.
    #[serde(default)]
    pub cost: f64,
    /// `cost` is an API-price estimate, not a charge (a claude.ai plan).
    #[serde(default)]
    pub cost_estimate: bool,
    #[serde(default)]
    pub credits: u64,
    #[serde(default)]
    pub ms: u64,
    /// What each tool's results added to the context, by [`tool_key`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolUse>,
}

/// The results of one tool (or one command) in a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolUse {
    #[serde(default)]
    pub calls: u64,
    /// Bytes of results the model was sent.
    #[serde(default)]
    pub bytes: u64,
}

impl ToolUse {
    pub fn add(&mut self, other: ToolUse) {
        self.calls += other.calls;
        self.bytes += other.bytes;
    }

    /// About four bytes a token, near enough to rank the tools.
    pub fn tokens(&self) -> u64 {
        self.bytes.div_ceil(4)
    }
}

/// The name a tool call is counted under: the tool's, and for `bash` the
/// program the command runs too (`bash cargo`), so noisy commands stand out.
pub fn tool_key(name: &str, command: Option<&str>) -> String {
    match command.and_then(program) {
        Some(p) if name == "bash" => format!("bash {p}"),
        _ => name.to_string(),
    }
}

/// The program a shell line runs: its first word past `VAR=value`
/// assignments, `cd <dir> &&`, `env`, `sudo`, `time` and `exec`, without its
/// directory. `cd ~/x && cargo test` runs cargo, not cd.
fn program(command: &str) -> Option<String> {
    let mut rest = command.trim();
    // The last `cd … &&` (or `;`) prefix: what runs is after it.
    while let Some(after) = rest
        .strip_prefix("cd ")
        .or_else(|| rest.strip_prefix("pushd "))
    {
        let cut = ["&&", ";"]
            .iter()
            .filter_map(|sep| after.find(sep).map(|i| i + sep.len()))
            .min()?;
        rest = after[cut..].trim_start();
    }
    let mut words = rest.split_whitespace();
    let mut wrapper: Option<&str> = None;
    let word = loop {
        let w = words.next()?;
        if w.contains('=') && !w.starts_with('=') {
            continue;
        }
        // A wrapper's options, and the argument of those that take one
        // (`sudo -u me`, `env -u VAR`), come before the program it runs.
        if let Some(wr) = wrapper
            && w.starts_with('-')
        {
            if takes_arg(wr, w) {
                words.next();
            }
            continue;
        }
        if matches!(
            w,
            "env" | "sudo" | "time" | "exec" | "command" | "nice" | "nohup"
        ) {
            wrapper = Some(w);
            continue;
        }
        break w;
    };
    let word = word.trim_start_matches(['(', '{']);
    let word = word.rsplit('/').next().unwrap_or(word);
    (!word.is_empty()).then(|| word.to_string())
}

/// Whether the short option `opt` of `wrapper` takes the next word as its argument.
fn takes_arg(wrapper: &str, opt: &str) -> bool {
    let takes: &[&str] = match wrapper {
        "sudo" => &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"],
        "env" => &["-u", "-C", "-S"],
        "time" => &["-o", "-f"],
        "nice" => &["-n"],
        "exec" => &["-a"],
        _ => &[],
    };
    takes.contains(&opt)
}

/// What each tool's results used over `turns`, largest first.
pub fn by_tool<'a>(turns: impl IntoIterator<Item = &'a Turn>) -> Vec<(String, ToolUse)> {
    let mut all: BTreeMap<String, ToolUse> = BTreeMap::new();
    for t in turns {
        for (k, u) in &t.tools {
            all.entry(k.clone()).or_default().add(*u);
        }
    }
    let mut v: Vec<_> = all.into_iter().collect();
    v.sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes).then(a.0.cmp(&b.0)));
    v
}

impl Turn {
    pub fn is_empty(&self) -> bool {
        self.input + self.output + self.cache_read + self.cache_write + self.credits == 0
            && self.cost <= 0.
            && self.tools.is_empty()
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("ARAKUN_USAGE_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::shared(crate::paths::USAGE))
}

/// About 60,000 turns. Past it the log moves to `usage.old.jsonl`, so the
/// two keep the latest turns without growing for ever.
const MAX_BYTES: u64 = 16 << 20;

pub fn append(turn: &Turn) -> Result<()> {
    let mut line = serde_json::to_vec(turn)?;
    line.push(b'\n');
    crate::rotate::append(&path(), &line, MAX_BYTES)?;
    Ok(())
}

/// Every turn still logged, oldest first. A line that does not parse, such
/// as one cut short by a crash, is skipped and the lines after it still read.
pub fn read() -> Vec<Turn> {
    let path = path();
    [crate::rotate::old_path(&path), path]
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .flat_map(|bytes| parse(&bytes))
        .collect()
}

fn parse(bytes: &[u8]) -> Vec<Turn> {
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|l| serde_json::from_slice(l).ok())
        .collect()
}

pub const USAGE: &str = "arakun usage tools [--days N] [--json]";

/// `arakun usage …`: reads the log, so it needs no running Arakun.
pub fn cli(args: &[String]) -> i32 {
    let mut days: Option<u64> = None;
    let mut json = false;
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("tools") => {}
        Some("-h" | "--help" | "help") => {
            println!("usage: {USAGE}\n\n{HELP}");
            return 0;
        }
        _ => {
            eprintln!("usage: {USAGE}");
            return 2;
        }
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--days" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => days = Some(n),
                None => {
                    eprintln!("usage: {USAGE}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("usage: {USAGE}\n\n{HELP}");
                return 0;
            }
            _ => {
                eprintln!("usage: {USAGE}");
                return 2;
            }
        }
    }
    let since = days.map_or(0, |d| super::rpc::unix_now().saturating_sub(d * 24 * 3600));
    let turns: Vec<Turn> = read().into_iter().filter(|t| t.at >= since).collect();
    let rows = by_tool(&turns);
    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|(k, u)| serde_json::json!({"tool": k, "calls": u.calls, "bytes": u.bytes, "tokens": u.tokens()}))
            .collect();
        println!("{}", serde_json::Value::Array(rows));
    } else {
        print!("{}", table(&rows));
    }
    0
}

const HELP: &str = "\
Where the tokens of tool results go: what each tool's results added to the
context of Workers AI sessions, from the usage log. bash is split by the
program the command runs. Tokens are bytes / 4. --days N counts the last N
days only; --json prints the rows as JSON.";

fn table(rows: &[(String, ToolUse)]) -> String {
    if rows.is_empty() {
        return "No tool results logged yet. Workers AI sessions log them as they run.\n".into();
    }
    let total: u64 = rows.iter().map(|r| r.1.bytes).sum::<u64>().max(1);
    let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(4);
    let mut out = format!(
        "{:<width$}  {:>7}  {:>10}  {:>5}\n",
        "tool", "calls", "tokens", "share"
    );
    for (k, u) in rows {
        out.push_str(&format!(
            "{k:<width$}  {:>7}  {:>10}  {:>4}%\n",
            u.calls,
            u.tokens(),
            u.bytes * 100 / total
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_round_trip_and_old_lines_still_read() {
        let t = Turn {
            at: 1,
            agent: AgentKind::Claude,
            session: "s".into(),
            cwd: "/p".into(),
            model: Some("claude-opus-4-7".into()),
            input: 10,
            output: 5,
            cache_read: 100,
            cache_write: 20,
            cost: 0.25,
            cost_estimate: true,
            credits: 0,
            ms: 900,
            tools: BTreeMap::from([(
                "bash cargo".to_string(),
                ToolUse {
                    calls: 2,
                    bytes: 4000,
                },
            )]),
        };
        let line = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Turn>(&line).unwrap(), t);
        let bare: Turn = serde_json::from_str(
            r#"{"at":2,"agent":"droid","session":"x","cwd":"/q","credits":7}"#,
        )
        .unwrap();
        assert_eq!(
            (bare.credits, bare.cost, bare.model.as_deref()),
            (7, 0., None)
        );
        assert!(!bare.is_empty());
        let tools_only: Turn = serde_json::from_str(
            r#"{"at":3,"agent":"workers-ai","session":"y","cwd":"/q","tools":{"grep":{"calls":1,"bytes":9}}}"#,
        )
        .unwrap();
        assert!(!tools_only.is_empty());
        assert!(bare.tools.is_empty());
        assert!(!serde_json::to_string(&bare).unwrap().contains("tools"));
    }

    #[test]
    fn bash_is_counted_by_the_program_it_runs() {
        let k = |c: &str| tool_key("bash", Some(c));
        assert_eq!(k("cargo test --locked"), "bash cargo");
        assert_eq!(k("cd /x/y && npm ci"), "bash npm");
        assert_eq!(k("cd a; cd b && git status"), "bash git");
        assert_eq!(k("RUST_LOG=1 FOO=a=b /usr/bin/git log"), "bash git");
        assert_eq!(k("sudo env X=1 make"), "bash make");
        assert_eq!(k("(grep -rn x src)"), "bash grep");
        assert_eq!(k("sudo -n cargo test"), "bash cargo");
        assert_eq!(k("env -u RUSTFLAGS cargo build"), "bash cargo");
        assert_eq!(k("sudo -u me -- make"), "bash make");
        assert_eq!(k("   "), "bash");
        assert_eq!(k("cd /x"), "bash");
        assert_eq!(tool_key("read_file", None), "read_file");
        assert_eq!(tool_key("grep", Some("cargo")), "grep");
    }

    #[test]
    fn tools_add_up_over_turns_largest_first() {
        let turn = |tools: &[(&str, u64, u64)]| Turn {
            at: 1,
            agent: AgentKind::WorkersAi,
            session: "s".into(),
            cwd: "/p".into(),
            model: None,
            input: 1,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: 0.,
            cost_estimate: false,
            credits: 0,
            ms: 0,
            tools: tools
                .iter()
                .map(|&(k, calls, bytes)| (k.to_string(), ToolUse { calls, bytes }))
                .collect(),
        };
        let turns = [
            turn(&[("read_file", 1, 100), ("bash cargo", 1, 50)]),
            turn(&[("bash cargo", 2, 400)]),
        ];
        let rows = by_tool(&turns);
        assert_eq!(
            rows[0],
            (
                "bash cargo".into(),
                ToolUse {
                    calls: 3,
                    bytes: 450
                }
            )
        );
        assert_eq!(
            rows[1],
            (
                "read_file".into(),
                ToolUse {
                    calls: 1,
                    bytes: 100
                }
            )
        );
        let shown = table(&rows);
        assert!(
            shown.contains("bash cargo") && shown.contains("81%"),
            "{shown}"
        );
    }

    #[test]
    fn a_bad_line_does_not_hide_the_turns_after_it() {
        let ok = r#"{"at":1,"agent":"droid","session":"x","cwd":"/q","credits":1}"#;
        let bytes = [ok.as_bytes(), b"\n{\"at\":2,\xff\n", ok.as_bytes(), b"\n"].concat();
        assert_eq!(parse(&bytes).len(), 2);
    }
}
