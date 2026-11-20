//! What a sandboxed agent can't do itself, it asks for: `request_action`
//! names an action (send an email, push a branch…), the user approves it in
//! the island or the terminal, and the host runs it with the user's own
//! credentials, outside the sandbox. `read_email` (with `--allow mail-read`)
//! has the host read mail for it, so the mailbox's password never goes in.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use super::sandbox::{Cap, Jail, run_with_timeout};

/// How the host shows these calls, and how the policy knows one.
pub const ACTION_TITLE: &str = "request_action";

pub const KINDS: &[&str] = &[
    "send_email",
    "git_push",
    "gh",
    #[cfg(feature = "coderlabs")]
    "okc_submit",
    "run_on_host",
];

/// The kinds only some builds have, as `request_action`'s description lists them.
#[cfg(feature = "coderlabs")]
const MORE_KINDS: &str = "- okc_submit: problem, file (inside the workspace)\n";
#[cfg(not(feature = "coderlabs"))]
const MORE_KINDS: &str = "";
/// And as the system prompt names them, after "open or comment on a pull request".
#[cfg(feature = "coderlabs")]
const MORE_ACTIONS: &str = ", submit to OKC";
#[cfg(not(feature = "coderlabs"))]
const MORE_ACTIONS: &str = "";

/// What the model is sent of an action's output at most.
const OUTPUT_CHARS: usize = 20_000;

pub fn tool_specs(caps: &[Cap]) -> Vec<Value> {
    let s = |d: &str| json!({"type": "string", "description": d});
    let mut specs = vec![json!({"type": "function", "function": {
        "name": "request_action",
        "description": format!("Ask the user to let arakun do something your sandbox can't: it has no network and no credentials. \
             The user approves or denies each request; on approval arakun runs it on the user's machine with the user's \
             credentials and you get its output. Kinds and their fields:\n\
             - send_email: to, cc?, subject, body, attachments? (paths inside the workspace)\n\
             - git_push: repo (a folder inside the workspace), remote (a remote name or URL), branch\n\
             - gh: args, only [\"pr\",\"create\",…] (with --repo and --head) or [\"pr\",\"comment\",<number>,…] (with --repo); body inline\n\
             {MORE_KINDS}\
             - run_on_host: command, why (a shell command outside the sandbox; only when nothing else will do)"),
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": KINDS},
            "to": {"type": "array", "items": {"type": "string"}, "description": "send_email: recipients."},
            "cc": {"type": "array", "items": {"type": "string"}, "description": "send_email: carbon copies."},
            "subject": s("send_email: the subject."),
            "body": s("send_email: the plain-text body."),
            "attachments": {"type": "array", "items": {"type": "string"}, "description": "send_email: files inside the workspace."},
            "repo": s("git_push: the repository folder."),
            "remote": s("git_push: remote name (origin) or URL."),
            "branch": s("git_push: the local branch, pushed to the same name."),
            "args": {"type": "array", "items": {"type": "string"}, "description": "gh: the arguments after `gh`."},
            "command": s("run_on_host: the shell command."),
            "why": s("Why you need it, for the user."),
        }, "required": ["action"]},
    }})];
    #[cfg(feature = "coderlabs")]
    {
        let props = &mut specs[0]["function"]["parameters"]["properties"];
        props["problem"] = s("okc_submit: the problem id.");
        props["file"] = s("okc_submit: the kernel file.");
    }
    if caps.contains(&Cap::MailRead) {
        specs.push(json!({"type": "function", "function": {
            "name": "read_email",
            "description": "Read the user's mail (read-only; arakun runs himalaya for you). With id: that message, \
                 without marking it read. With query: matching envelopes, in himalaya's search DSL \
                 (`from x and subject y`, `after 2026-10-01`, `order by date desc`). With neither: the latest envelopes.",
            "parameters": {"type": "object", "properties": {
                "id": s("A message id from an envelope list."),
                "query": s("A search query."),
                "mailbox": s("Mailbox (default the inbox)."),
            }, "required": []},
        }}));
    }
    specs
}

/// What a headless agent's system prompt adds about where it runs.
pub fn prompt(jail: &Jail) -> String {
    let net = jail.caps.iter().any(|c| matches!(c, Cap::Net | Cap::Modal));
    let mut s = String::from("\n\nYou run headless, unattended. ");
    if jail.sandbox.is_some() {
        s.push_str(&format!(
            "Your tools run in a sandbox: you can change files only in {ws} (and /tmp); the rest of the machine is \
             read-only, credentials (SSH keys, tokens, mail, cloud and git logins) are not there, the `arakun` command \
             can't reach the app, and {net}. Don't try to work around it: it is not a bug to fix.",
            ws = jail.workspace.display(),
            net = if net { "the network is open but has no credentials" } else { "there is no network" },
        ));
    } else {
        s.push_str(
            "The user let you run without a sandbox; still, never send, push or publish from bash.",
        );
    }
    s.push_str(
        &format!(
            " To send an email, push a branch, open or comment on a pull request{MORE_ACTIONS}, or run something on \
             the user's machine, call request_action: the user approves it and arakun runs it. If it is denied, \
             don't try another way."
        ),
    );
    if jail.caps.contains(&Cap::Modal) {
        s.push_str(" Modal works from the sandbox (its token is in ~/.modal.toml).");
    }
    if jail.caps.contains(&Cap::MailRead) {
        s.push_str(" read_email reads the user's mail for you.");
    }
    s
}

fn text(args: &Value, k: &str) -> Option<String> {
    args[k]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn list(args: &Value, k: &str) -> Vec<String> {
    match &args[k] {
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn address(a: &str) -> Result<String, String> {
    let ok = a.contains('@')
        && !a.starts_with('-')
        && !a.chars().any(|c| c.is_control() || c == ',' || c == ';')
        && a.split('@').all(|p| !p.trim().is_empty());
    if ok {
        Ok(a.to_string())
    } else {
        Err(format!("not an email address: {a:?}"))
    }
}

/// A file the action may take: inside the workspace, really, and not hidden.
fn inside(jail: &Jail, raw: &str) -> Result<PathBuf, String> {
    let p = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        jail.workspace.join(raw)
    };
    let real = crate::platform::canonicalize(&p).map_err(|e| format!("{raw}: {e}"))?;
    if !real.starts_with(&jail.workspace) {
        return Err(format!(
            "{raw} is not inside the workspace {}",
            jail.workspace.display()
        ));
    }
    if let Some(sb) = &jail.sandbox
        && sb.hidden.iter().any(|h| real.starts_with(&h.path))
    {
        return Err(format!("{raw} is hidden from the sandbox"));
    }
    Ok(real)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::platform::command("git");
    cmd.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "protocol.ext.allow=never",
        "-C",
    ])
    .arg(dir)
    .args(args)
    .env("GIT_TERMINAL_PROMPT", "0");
    let out = run_with_timeout(cmd, None, Duration::from_secs(30))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The flags `gh pr create|comment` may carry, and whether each takes a value.
const GH_FLAGS: &[(&str, bool)] = &[
    ("--title", true),
    ("-t", true),
    ("--body", true),
    ("-b", true),
    ("--base", true),
    ("-B", true),
    ("--head", true),
    ("-H", true),
    ("--repo", true),
    ("-R", true),
    ("--draft", false),
    ("-d", false),
    ("--label", true),
    ("-l", true),
    ("--reviewer", true),
    ("-r", true),
];

fn gh_value<'a>(args: &'a [String], names: &[&str]) -> Option<&'a str> {
    args.windows(2)
        .find(|w| names.contains(&w[0].as_str()))
        .map(|w| w[1].as_str())
}

/// The model's request checked and pinned down: what the user approves is
/// exactly what runs. Carries `arakun_summary`, `arakun_phrase` and `arakun_why`
/// for the pending request.
pub fn prepare(args: &Value, jail: &Jail) -> Result<Value, String> {
    let kind = text(args, "action").ok_or("action is missing")?;
    let why = text(args, "why").map(|w| clip(&w, 300));
    let (mut v, summary, phrase) = match kind.as_str() {
        "send_email" => {
            let to = list(args, "to")
                .iter()
                .map(|a| address(a))
                .collect::<Result<Vec<_>, _>>()?;
            let cc = list(args, "cc")
                .iter()
                .map(|a| address(a))
                .collect::<Result<Vec<_>, _>>()?;
            if to.is_empty() {
                return Err("send_email needs to".into());
            }
            let subject = text(args, "subject").ok_or("send_email needs a subject")?;
            if subject.chars().any(char::is_control) {
                return Err("the subject has a line break".into());
            }
            let body = args["body"]
                .as_str()
                .filter(|b| !b.trim().is_empty())
                .ok_or("send_email needs a body")?
                .to_string();
            let attachments = list(args, "attachments")
                .iter()
                .map(|a| inside(jail, a))
                .collect::<Result<Vec<_>, _>>()?;
            for a in &attachments {
                if !a.is_file() {
                    return Err(format!("{} is not a file", a.display()));
                }
            }
            let more = if to.len() > 1 {
                format!(" and {} more", to.len() - 1)
            } else {
                String::new()
            };
            let mut summary = format!(
                "Send mail to {}{more} with subject \"{}\"",
                to.join(", "),
                clip(&subject, 80)
            );
            if !cc.is_empty() {
                summary.push_str(&format!(" (cc: {})", cc.join(", ")));
            }
            if !attachments.is_empty() {
                let names: Vec<String> = attachments
                    .iter()
                    .map(|a| {
                        a.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                summary.push_str(&format!(", attachments: {}", names.join(", ")));
            }
            let v = json!({"action": kind, "to": to, "cc": cc, "subject": subject, "body": body, "attachments": attachments});
            (v, summary, format!("confirm: send mail to {}{more}", to[0]))
        }
        "git_push" => {
            let repo = inside(jail, &text(args, "repo").unwrap_or_else(|| ".".into()))?;
            let remote = text(args, "remote").ok_or("git_push needs a remote")?;
            let branch = text(args, "branch").ok_or("git_push needs a branch")?;
            if remote.starts_with('-') || branch.starts_with('-') {
                return Err("remote and branch can't start with -".into());
            }
            git(&repo, &["check-ref-format", "--branch", &branch])
                .map_err(|_| format!("not a branch name: {branch}"))?;
            let url = git(&repo, &["remote", "get-url", "--push", "--", &remote])
                .unwrap_or_else(|_| remote.clone());
            if url.starts_with("ext::") || url.starts_with('-') || url.contains(['\n', '\r']) {
                return Err(format!("arakun won't push to {url}"));
            }
            // A path the repo's config gives relative to the repo; the push runs elsewhere.
            let scp = url.find(':').is_some_and(|i| !url[..i].contains('/'));
            let url = if !scp && Path::new(&url).is_relative() {
                crate::platform::canonicalize(repo.join(&url))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(url)
            } else {
                url
            };
            let sha = git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("refs/heads/{branch}^{{commit}}"),
                ],
            )
            .map_err(|e| format!("no branch {branch} in {}: {e}", repo.display()))?;
            let subject = git(&repo, &["log", "-1", "--format=%s", &sha]).unwrap_or_default();
            let v = json!({"action": kind, "repo": repo, "remote": remote, "url": url, "branch": branch, "sha": sha});
            let summary = format!(
                "Push the branch {branch} ({} \"{}\") to {url}",
                &sha[..sha.len().min(10)],
                clip(&subject, 60)
            );
            (
                v,
                summary,
                format!("confirm: git push {branch} to {remote}"),
            )
        }
        "gh" => {
            let a = list(args, "args");
            let (verb, rest) = match a.as_slice() {
                [p, v, rest @ ..] if p == "pr" && (v == "create" || v == "comment") => {
                    (v.clone(), rest)
                }
                _ => return Err("gh takes only pr create or pr comment".into()),
            };
            let mut i = 0;
            let mut positional = Vec::new();
            while i < rest.len() {
                match GH_FLAGS.iter().find(|(f, _)| *f == rest[i]) {
                    Some((_, true)) if i + 1 < rest.len() => i += 2,
                    Some((_, false)) => i += 1,
                    Some(_) => return Err(format!("{} needs a value", rest[i])),
                    None if rest[i].starts_with('-') => {
                        return Err(format!(
                            "gh flag not allowed: {} (put the body inline)",
                            rest[i]
                        ));
                    }
                    None => {
                        positional.push(rest[i].clone());
                        i += 1;
                    }
                }
            }
            let repo = gh_value(rest, &["--repo", "-R"])
                .ok_or("gh needs --repo OWNER/REPO")?
                .to_string();
            let summary = if verb == "create" {
                if gh_value(rest, &["--head", "-H"]).is_none() || !positional.is_empty() {
                    return Err("gh pr create needs --head and no positional arguments".into());
                }
                format!(
                    "Open a pull request in {repo}: \"{}\"",
                    clip(gh_value(rest, &["--title", "-t"]).unwrap_or(""), 80)
                )
            } else {
                if positional.len() != 1 || gh_value(rest, &["--body", "-b"]).is_none() {
                    return Err("gh pr comment needs one pull request and --body".into());
                }
                format!(
                    "Comment on pull request {} in {repo}: \"{}\"",
                    positional[0],
                    clip(gh_value(rest, &["--body", "-b"]).unwrap_or(""), 80)
                )
            };
            (
                json!({"action": kind, "args": a}),
                summary,
                format!("confirm: gh pr {verb}"),
            )
        }
        #[cfg(feature = "coderlabs")]
        "okc_submit" => {
            let problem = text(args, "problem").ok_or("okc_submit needs a problem")?;
            if !problem
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!("not a problem id: {problem}"));
            }
            let file = inside(jail, &text(args, "file").ok_or("okc_submit needs a file")?)?;
            let summary = format!("Enviar {} a OKC (problema {problem})", file.display());
            (
                json!({"action": kind, "problem": problem, "file": file}),
                summary,
                "confirmo: enviar a okc".to_string(),
            )
        }
        "run_on_host" => {
            let command = text(args, "command").ok_or("run_on_host needs a command")?;
            if why.is_none() {
                return Err("run_on_host needs why".into());
            }
            let summary = format!(
                "Run on your computer, outside the sandbox: {}",
                clip(&command, 200)
            );
            (
                json!({"action": kind, "host_command": command}),
                summary,
                "confirm: run on my computer".to_string(),
            )
        }
        other => {
            return Err(format!(
                "unknown action {other}; one of: {}",
                KINDS.join(", ")
            ));
        }
    };
    v["arakun_summary"] = json!(summary);
    v["arakun_phrase"] = json!(phrase);
    v["arakun_why"] = json!(why.unwrap_or_else(|| format!(
        "{kind}: arakun does it with your credentials, outside the sandbox"
    )));
    Ok(v)
}

/// Finds a program on the host's PATH.
type Find<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;

fn program(find: Find, name: &str, fallback: Option<PathBuf>) -> Result<PathBuf, String> {
    find(name)
        .or(fallback.filter(|p| p.exists()))
        .ok_or_else(|| format!("{name} is not installed"))
}

fn finish(out: std::process::Output) -> Result<String, String> {
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str(&format!("\n[stderr]\n{}", err.trim_end()));
    }
    let text = clip(&text, OUTPUT_CHARS);
    if out.status.success() {
        Ok(if text.is_empty() {
            "(done, no output)".into()
        } else {
            text
        })
    } else {
        Err(format!(
            "exit {}: {text}",
            out.status
                .code()
                .map_or("by a signal".into(), |c| c.to_string())
        ))
    }
}

fn scratch(what: &str) -> Result<PathBuf, String> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("arakun-{what}-{}-{t}", std::process::id()));
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

/// Runs an approved action on the host; what comes back goes to the model.
pub fn execute(v: &Value, workspace: &Path) -> Result<String, String> {
    execute_with(v, workspace, &crate::agent::which)
}

fn execute_with(v: &Value, workspace: &Path, find: Find) -> Result<String, String> {
    let strs = |k: &str| {
        v[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let str_of = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    match v["action"].as_str().unwrap_or_default() {
        "send_email" => {
            let mut cmd = crate::platform::command(program(find, "himalaya", None)?);
            cmd.args(["message", "compose"]);
            for a in strs("to") {
                cmd.arg("--to").arg(a);
            }
            for a in strs("cc") {
                cmd.arg("--cc").arg(a);
            }
            cmd.arg("--subject").arg(str_of("subject"));
            for a in strs("attachments") {
                cmd.arg("--attach").arg(a);
            }
            cmd.arg("--send").current_dir(workspace);
            finish(run_with_timeout(
                cmd,
                Some(str_of("body").as_bytes()),
                Duration::from_secs(120),
            )?)
        }
        "git_push" => {
            let (repo, url, branch, sha) = (
                str_of("repo"),
                str_of("url"),
                str_of("branch"),
                str_of("sha"),
            );
            // From a clean copy, so the workspace's .git/config and hooks have no say.
            let tmp = scratch("push")?;
            let result = (|| {
                git(&tmp, &["init", "--bare", "-q"])?;
                git(
                    &tmp,
                    &[
                        "-c",
                        "protocol.file.allow=always",
                        "fetch",
                        "-q",
                        "--no-tags",
                        "--",
                        &repo,
                        &format!("+refs/heads/{branch}:refs/heads/{branch}"),
                    ],
                )?;
                let got = git(&tmp, &["rev-parse", &format!("refs/heads/{branch}")])?;
                if got != sha {
                    return Err(format!(
                        "{branch} moved since you approved it ({sha} → {got}); nothing was pushed"
                    ));
                }
                let mut cmd = crate::platform::command("git");
                cmd.args([
                    "-c",
                    "protocol.ext.allow=never",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-C",
                ])
                .arg(&tmp)
                .args([
                    "push",
                    "--porcelain",
                    "--",
                    &url,
                    &format!("{sha}:refs/heads/{branch}"),
                ])
                .env("GIT_TERMINAL_PROMPT", "0");
                finish(run_with_timeout(cmd, None, Duration::from_secs(300))?)
            })();
            let _ = std::fs::remove_dir_all(&tmp);
            result
        }
        "gh" => {
            let tmp = scratch("gh")?;
            let mut cmd = crate::platform::command(program(find, "gh", None)?);
            cmd.args(strs("args"))
                .current_dir(&tmp)
                .env("GH_PROMPT_DISABLED", "1");
            let out = run_with_timeout(cmd, None, Duration::from_secs(120));
            let _ = std::fs::remove_dir_all(&tmp);
            finish(out?)
        }
        #[cfg(feature = "coderlabs")]
        "okc_submit" => {
            let okc = program(
                find,
                "okc",
                Some(crate::platform::home().join(".okc/bin/okc")),
            )?;
            let mut cmd = crate::platform::command(okc);
            cmd.args(["submit", &str_of("problem"), &str_of("file"), "--json"])
                .current_dir(workspace);
            finish(run_with_timeout(cmd, None, Duration::from_secs(3600))?)
        }
        "run_on_host" => {
            let mut cmd = crate::platform::command("bash");
            cmd.arg("-c")
                .arg(str_of("host_command"))
                .current_dir(workspace);
            finish(run_with_timeout(cmd, None, Duration::from_secs(600))?)
        }
        other => Err(format!("unknown action {other}")),
    }
}

/// `read_email`, run by the host.
pub fn read_email(args: &Value) -> Result<String, String> {
    let mut cmd: Command =
        crate::platform::command(program(&crate::agent::which, "himalaya", None)?);
    let mailbox = text(args, "mailbox");
    let id = text(args, "id");
    if let Some(id) = &id {
        if id.starts_with('-') {
            return Err(format!("not a message id: {id}"));
        }
        cmd.args(["message", "read"]);
        if let Some(m) = &mailbox {
            cmd.arg("--mailbox").arg(m);
        }
        cmd.arg("--").arg(id);
    } else {
        match text(args, "query") {
            Some(q) => {
                cmd.args(["envelope", "search", "--page-size", "30"]);
                if let Some(m) = &mailbox {
                    cmd.arg("--mailbox").arg(m);
                }
                cmd.arg("--").args(q.split_whitespace());
            }
            None => {
                cmd.args(["envelope", "list", "--page-size", "30"]);
                if let Some(m) = &mailbox {
                    cmd.arg("--mailbox").arg(m);
                }
            }
        }
    }
    finish(run_with_timeout(cmd, None, Duration::from_secs(60))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jail(ws: &Path) -> Jail {
        Jail {
            sandbox: None,
            caps: Vec::new(),
            workspace: crate::platform::canonicalize(ws).unwrap(),
        }
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arakun-broker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn emails_are_pinned_down_with_their_phrase() {
        let ws = tmpdir("mail");
        std::fs::write(ws.join("report.pdf"), "x").unwrap();
        let j = jail(&ws);
        let v = prepare(
            &json!({"action": "send_email", "to": ["juan@x.com"], "subject": "Hola", "body": "cuerpo", "attachments": ["report.pdf"], "arakun_summary": "nada"}),
            &j,
        )
        .unwrap();
        assert_eq!(v["arakun_phrase"], "confirm: send mail to juan@x.com");
        assert!(
            v["arakun_summary"]
                .as_str()
                .unwrap()
                .starts_with("Send mail to juan@x.com with subject \"Hola\"")
        );
        assert_eq!(v["attachments"][0], json!(j.workspace.join("report.pdf")));
        let v = prepare(
            &json!({"action": "send_email", "to": "a@x.com, b@y.com", "subject": "S", "body": "b"}),
            &j,
        )
        .unwrap();
        assert_eq!(
            v["arakun_phrase"],
            "confirm: send mail to a@x.com and 1 more"
        );
        let bad = |a: Value| prepare(&a, &j).unwrap_err();
        assert!(
            bad(json!({"action": "send_email", "to": ["x"], "subject": "S", "body": "b"}))
                .contains("not an email")
        );
        assert!(bad(json!({"action": "send_email", "to": ["a@x.com"], "subject": "S\nBcc: z@z", "body": "b"})).contains("line break"));
        let away = tmpdir("mail-away");
        let outside = away.join("secret.txt");
        std::fs::write(&outside, "x").unwrap();
        assert!(bad(json!({"action": "send_email", "to": ["a@x.com"], "subject": "S", "body": "b", "attachments": [outside]})).contains("not inside"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, ws.join("link")).unwrap();
            assert!(bad(json!({"action": "send_email", "to": ["a@x.com"], "subject": "S", "body": "b", "attachments": ["link"]})).contains("not inside"));
        }
        assert!(bad(json!({"action": "teleport"})).contains("unknown action"));
        std::fs::remove_dir_all(&ws).unwrap();
        std::fs::remove_dir_all(&away).unwrap();
    }

    #[test]
    fn gh_takes_only_pull_requests() {
        let j = jail(&std::env::temp_dir());
        let ok = |a: &[&str]| prepare(&json!({"action": "gh", "args": a}), &j);
        assert_eq!(
            ok(&[
                "pr", "create", "--repo", "o/r", "--head", "b", "--title", "T", "--body", "B"
            ])
            .unwrap()["arakun_phrase"],
            "confirm: gh pr create"
        );
        assert!(ok(&["pr", "comment", "12", "--repo", "o/r", "--body", "hi"]).is_ok());
        assert!(ok(&["pr", "merge", "12", "--repo", "o/r"]).is_err());
        assert!(ok(&["api", "-X", "DELETE", "repos/o/r"]).is_err());
        assert!(
            ok(&[
                "pr",
                "create",
                "--repo",
                "o/r",
                "--head",
                "b",
                "--body-file",
                "/etc/passwd"
            ])
            .is_err()
        );
        assert!(ok(&["pr", "create", "--head", "b"]).is_err());
        assert!(
            ok(&[
                "pr",
                "comment",
                "12",
                "--repo",
                "o/r",
                "--body",
                "x",
                "--edit-last"
            ])
            .is_err()
        );
    }

    #[test]
    fn host_commands_need_a_reason_and_hide_from_the_classifier() {
        let j = jail(&std::env::temp_dir());
        assert!(prepare(&json!({"action": "run_on_host", "command": "ls"}), &j).is_err());
        let v = prepare(
            &json!({"action": "run_on_host", "command": "ls", "why": "ver"}),
            &j,
        )
        .unwrap();
        assert!(v.get("command").is_none());
        assert_eq!(v["host_command"], "ls");
        assert_eq!(v["arakun_why"], "ver");
    }

    #[cfg(unix)]
    fn fake(dir: &Path, name: &str, log: &Path) {
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\ncat >> '{}'\necho sent\n",
                log.display(),
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn approved_actions_run_on_the_host() {
        let ws = tmpdir("exec");
        let bin = ws.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = ws.join("log");
        fake(&bin, "himalaya", &log);
        let j = jail(&ws);
        let v = prepare(&json!({"action": "send_email", "to": ["juan@x.com"], "cc": ["ana@y.com"], "subject": "Hola", "body": "cuerpo\n"}), &j).unwrap();
        let out = execute_with(&v, &j.workspace, &|n| {
            Some(bin.join(n)).filter(|p| p.exists())
        });
        assert_eq!(out.unwrap().trim(), "sent");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            logged,
            "message\ncompose\n--to\njuan@x.com\n--cc\nana@y.com\n--subject\nHola\n--send\ncuerpo\n"
        );
        std::fs::remove_dir_all(&ws).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pushes_go_from_a_clean_copy_of_what_was_approved() {
        let ws = tmpdir("push");
        let sh = |dir: &Path, c: &str| {
            let out = crate::git::unpinned(&mut Command::new("sh"))
                .arg("-c")
                .arg(c)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{c}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        sh(
            &ws,
            "git init -q --bare remote.git && git init -q -b main repo && cd repo && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m one && git remote add origin ../remote.git",
        );
        // A hook the agent planted must not run on the host.
        sh(
            &ws,
            "printf '#!/bin/sh\\ntouch ../pwned\\n' > repo/.git/hooks/pre-push && chmod +x repo/.git/hooks/pre-push",
        );
        let j = jail(&ws);
        let v = prepare(
            &json!({"action": "git_push", "repo": "repo", "remote": "origin", "branch": "main"}),
            &j,
        )
        .unwrap();
        assert_eq!(v["arakun_phrase"], "confirm: git push main to origin");
        assert!(v["url"].as_str().unwrap().ends_with("remote.git"));
        execute(&v, &j.workspace).unwrap();
        assert!(!ws.join("pwned").exists());
        let pushed = crate::git::unpinned(&mut Command::new("git"))
            .args(["-C", "remote.git", "rev-parse", "main"])
            .current_dir(&ws)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&pushed.stdout).trim(),
            v["sha"].as_str().unwrap()
        );
        // A commit after the approval is not what was approved.
        sh(
            &ws,
            "cd repo && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m two",
        );
        assert!(execute(&v, &j.workspace).unwrap_err().contains("moved"));
        std::fs::remove_dir_all(&ws).unwrap();
    }
}
