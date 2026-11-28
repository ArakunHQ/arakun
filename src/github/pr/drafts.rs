//! Reviews an agent wrote, kept on disk until the user posts them, and `arakun github draft`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::*;
use crate::github::Problem;

/// A review an agent wrote and nobody sent: the user reads it in the pull
/// request view and posts, edits or discards it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub repo: String,
    pub number: u64,
    pub verdict: Verdict,
    pub body: String,
    /// Which agent wrote it.
    #[serde(default)]
    pub author: String,
    /// The head commit it was written against.
    #[serde(default)]
    pub head_sha: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
}

impl Draft {
    pub fn item(&self) -> ItemRef {
        ItemRef {
            repo: self.repo.clone(),
            number: self.number,
            kind: ItemKind::Pull,
        }
    }
}

fn drafts_dir() -> PathBuf {
    // Next to the state file, so a test Arakun keeps its drafts apart.
    crate::paths::store(crate::paths::GITHUB_DRAFTS)
}

/// One file per pull request. GitHub's names ignore case, so the file's does too.
fn draft_file(dir: &Path, repo: &str, number: u64) -> PathBuf {
    let safe: String = repo
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // `owner/name` becomes `owner_name`; the number keeps two names from colliding on a dot.
    dir.join(format!("{safe}#{number}.json"))
}

fn save_draft_in(dir: &Path, draft: &Draft) -> std::io::Result<()> {
    if !valid_repo(&draft.repo) || draft.number == 0 {
        return Err(std::io::Error::other(format!(
            "not a pull request: {}#{}",
            draft.repo, draft.number
        )));
    }
    let bytes = serde_json::to_vec_pretty(draft).map_err(std::io::Error::other)?;
    crate::durable::write(&draft_file(dir, &draft.repo, draft.number), &bytes)
}

fn load_draft_in(dir: &Path, item: &ItemRef) -> Option<Draft> {
    let bytes = std::fs::read(draft_file(dir, &item.repo, item.number)).ok()?;
    let draft: Draft = serde_json::from_slice(&bytes).ok()?;
    // `a/b.c` and `a.b/c` can't both exist on GitHub, but a file is cheap to check.
    (draft.repo.eq_ignore_ascii_case(&item.repo) && draft.number == item.number).then_some(draft)
}

fn delete_draft_in(dir: &Path, item: &ItemRef) -> std::io::Result<()> {
    match std::fs::remove_file(draft_file(dir, &item.repo, item.number)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Removes the draft only if it is still `posted`. The file is taken first, so
/// checking and removing are one step: a draft saved meanwhile lands on the
/// name, not on the file taken, and one taken by mistake goes back.
fn delete_draft_if_in(dir: &Path, posted: &Draft) -> std::io::Result<()> {
    use std::io::ErrorKind;
    let path = draft_file(dir, &posted.repo, posted.number);
    let mut taken = std::ffi::OsString::from(".");
    taken.push(path.file_name().unwrap_or_default());
    taken.push(format!(".{}.posted", std::process::id()));
    let taken = dir.join(taken);
    match std::fs::rename(&path, &taken) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    let held = std::fs::read(&taken)
        .ok()
        .and_then(|b| serde_json::from_slice::<Draft>(&b).ok());
    if held.as_ref() != Some(posted) {
        // Back where it was, unless a newer draft took the name meanwhile.
        match std::fs::hard_link(&taken, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            // No hard links here (FAT, some network drives): move it back.
            Err(_) if !path.exists() => return std::fs::rename(&taken, &path),
            Err(e) => return Err(e),
        }
    }
    std::fs::remove_file(&taken)
}

/// Keeps the draft, in place of any earlier one for the same pull request.
pub fn save_draft(draft: &Draft) -> std::io::Result<()> {
    save_draft_in(&drafts_dir(), draft)
}

pub fn load_draft(item: &ItemRef) -> Option<Draft> {
    load_draft_in(&drafts_dir(), item)
}

/// Removes the draft; fine when there is none.
pub fn delete_draft(item: &ItemRef) -> std::io::Result<()> {
    delete_draft_in(&drafts_dir(), item)
}

/// Removes the draft once posted, unless a new one replaced it meanwhile.
pub fn delete_draft_if(posted: &Draft) -> std::io::Result<()> {
    delete_draft_if_in(&drafts_dir(), posted)
}

/// Whether the pull request got new commits after the draft was written, so
/// the draft may talk about code that changed. A draft that doesn't know its
/// commit is not called stale.
pub fn draft_is_stale(draft: &Draft, pull: &Pull) -> bool {
    !draft.head_sha.is_empty()
        && !pull.head_ref_oid.is_empty()
        && draft.head_sha != pull.head_ref_oid
}

/// How a draft goes out. An approval counts only for the commit the agent
/// read: once the pull request has other commits, or the draft doesn't say
/// which one it read, it can go out as a comment but never as an approval.
pub fn draft_posts_as(draft: &Draft, pull: &Pull) -> Verdict {
    let current = !draft.head_sha.is_empty() && draft.head_sha == pull.head_ref_oid;
    if draft.verdict == Verdict::Approve && !current {
        Verdict::Comment
    } else {
        draft.verdict
    }
}

/// Posts a draft review as `verdict`. An approval first asks GitHub for the
/// head again: a push since the last read would otherwise be approved unseen.
pub fn post_draft(item: &ItemRef, draft: &Draft, verdict: Verdict) -> Result<(), Problem> {
    if verdict == Verdict::Approve {
        let now = head_sha(item)?;
        if now != draft.head_sha {
            return Err(invalid(&format!(
                "New commits were pushed after {}: nothing was approved. Read the draft against them, or post it as a comment.",
                short_sha(&draft.head_sha)
            )));
        }
    }
    // The check above words the usual case; the commit in the request is
    // what holds when a push lands between the two.
    review(item, verdict, &draft.body, &draft.head_sha)
}

impl DraftArgs {
    /// The commit the draft is for. An approval must say which commit the
    /// agent read: the head when the draft is saved may be a later push, and
    /// the approval would then go out for code nobody read. Other verdicts
    /// fall back to `current`, which only words the "new commits" hint.
    pub fn reviewed_head(
        &self,
        current: impl FnOnce() -> Result<String, String>,
    ) -> Result<String, String> {
        match (&self.head, self.verdict) {
            (Some(head), _) => Ok(head.clone()),
            (None, Verdict::Approve) => Err(format!(
                "an approval needs --head <sha>, the commit you read; read it before the diff with `gh pr view {} --json headRefOid --jq .headRefOid`",
                self.item.url()
            )),
            (None, _) => current(),
        }
    }

    /// The draft these arguments describe, with the review's text, the head
    /// commit it was written for, and the time now.
    pub fn draft(&self, body: String, head_sha: String, now: u64) -> Draft {
        Draft {
            repo: self.item.repo.clone(),
            number: self.item.number,
            verdict: self.verdict,
            body,
            author: self.agent.clone().unwrap_or_default(),
            head_sha,
            created: now,
        }
    }
}

/// What `arakun github draft` was asked to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftArgs {
    pub item: ItemRef,
    pub verdict: Verdict,
    /// Where the review's text is; `-` means stdin.
    pub body_file: PathBuf,
    /// `--agent <name>`: who wrote it, when the caller says.
    pub agent: Option<String>,
    /// `--head <sha>`: the commit the agent read.
    pub head: Option<String>,
}

pub const DRAFT_USAGE: &str = "usage: arakun github draft <owner/repo>#<n> --verdict <approve|request-changes|comment> --body-file <file|-> [--head <sha>] [--agent <name>]";

/// The arguments after `arakun github draft`. The pull request may also be
/// given as its address.
pub fn parse_draft_args(args: &[String]) -> Result<DraftArgs, String> {
    parse_draft_args_on(args, &gh_host_now())
}

/// `parse_draft_args` for URLs on `host`: tests name it instead of reading
/// `GH_HOST`, which other tests running at the same time may see.
fn parse_draft_args_on(args: &[String], gh: &str) -> Result<DraftArgs, String> {
    let (mut item, mut verdict, mut body_file, mut agent, mut head) =
        (None, None, None, None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        if !flag.starts_with("--") {
            if item.is_some() {
                return Err(format!("unexpected argument `{arg}`\n{DRAFT_USAGE}"));
            }
            let host = gh.to_string();
            let found = ItemRef::parse(arg).or_else(|| ItemRef::from_url_on(arg, &host));
            let found = found.ok_or_else(|| {
                if ItemRef::from_url(arg).is_some() {
                    format!("`{arg}` is not on {host}, the host gh works with; name it as owner/repo#number there")
                } else {
                    format!("`{arg}` is not a pull request (owner/repo#number)\n{DRAFT_USAGE}")
                }
            })?;
            if found.kind != ItemKind::Pull {
                return Err(format!(
                    "`{arg}` is an issue; only a pull request takes a review"
                ));
            }
            item = Some(found);
            continue;
        }
        if !matches!(flag, "--verdict" | "--body-file" | "--agent" | "--head") {
            return Err(format!("unknown option `{flag}`\n{DRAFT_USAGE}"));
        }
        let value = inline
            .or_else(|| args.next().cloned())
            .filter(|v| !v.is_empty());
        let value = value.ok_or_else(|| format!("`{flag}` needs a value\n{DRAFT_USAGE}"))?;
        match flag {
            "--verdict" => {
                let found = Verdict::from_name(&value);
                verdict = Some(found.ok_or_else(|| {
                    format!("`{value}` is not a verdict: approve, request-changes or comment")
                })?);
            }
            "--body-file" => body_file = Some(PathBuf::from(value)),
            "--head" if is_sha(&value) => head = Some(value.to_ascii_lowercase()),
            "--head" => {
                return Err(format!(
                    "`{value}` is not a full commit SHA (`gh pr view <url> --json headRefOid`)"
                ));
            }
            _ => agent = Some(value),
        }
    }
    let missing = |what: &str| format!("missing {what}\n{DRAFT_USAGE}");
    Ok(DraftArgs {
        item: item.ok_or_else(|| missing("the pull request"))?,
        verdict: verdict.ok_or_else(|| missing("--verdict"))?,
        body_file: body_file.ok_or_else(|| missing("--body-file"))?,
        agent,
        head,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

    #[test]
    fn drafts_are_kept_one_per_pull_request() {
        let dir = std::env::temp_dir().join(format!("arakun-drafts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pr = item(ItemKind::Pull);
        let draft = Draft {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            verdict: Verdict::RequestChanges,
            body: "The name is read too early.\n\n- `src/terminal.rs:42`".into(),
            author: "claude".into(),
            head_sha: "bbbbbbb2222222".into(),
            created: 1_794_000_000,
        };
        assert_eq!(load_draft_in(&dir, &pr), None);
        save_draft_in(&dir, &draft).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), Some(draft.clone()));
        assert_eq!(draft.item(), pr);
        // GitHub's names ignore case; another number is another draft.
        assert_eq!(
            load_draft_in(
                &dir,
                &ItemRef {
                    repo: "arakunhq/ARAKUN".into(),
                    ..pr.clone()
                }
            ),
            Some(draft.clone())
        );
        assert_eq!(
            load_draft_in(
                &dir,
                &ItemRef {
                    number: 42,
                    ..pr.clone()
                }
            ),
            None
        );

        // A second draft replaces the first.
        let again = Draft {
            verdict: Verdict::Approve,
            body: "Fixed.".into(),
            ..draft.clone()
        };
        save_draft_in(&dir, &again).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), Some(again.clone()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        // The stored form names the verdict the way the command line does.
        let text = std::fs::read_to_string(draft_file(&dir, "ArakunHQ/arakun", 41)).unwrap();
        assert!(text.contains("\"verdict\": \"approve\""), "{text}");
        assert!(draft_file(&dir, "ArakunHQ/arakun", 41).ends_with("arakunhq_arakun#41.json"));

        // Once posted, only the draft that was posted goes.
        delete_draft_if_in(&dir, &draft).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), Some(again.clone()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        delete_draft_if_in(&dir, &again).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), None);
        delete_draft_if_in(&dir, &again).unwrap();
        save_draft_in(&dir, &again).unwrap();

        delete_draft_in(&dir, &pr).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), None);
        delete_draft_in(&dir, &pr).unwrap();

        // Nothing that isn't a pull request's name becomes a file.
        assert!(
            save_draft_in(
                &dir,
                &Draft {
                    repo: "../../etc".into(),
                    ..draft.clone()
                }
            )
            .is_err()
        );
        assert!(
            save_draft_in(
                &dir,
                &Draft {
                    number: 0,
                    ..draft.clone()
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);

        // Stale once the head moves.
        let p = pull();
        assert!(!draft_is_stale(&draft, &p));
        assert!(draft_is_stale(
            &Draft {
                head_sha: "aaaaaaa1111111".into(),
                ..draft.clone()
            },
            &p
        ));
        assert!(!draft_is_stale(
            &Draft {
                head_sha: String::new(),
                ..draft
            },
            &p
        ));
    }

    #[test]
    fn a_draft_approves_only_the_commit_it_was_written_for() {
        let p = pull();
        let draft = Draft {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            verdict: Verdict::Approve,
            body: "LGTM".into(),
            author: "Claude".into(),
            head_sha: p.head_ref_oid.clone(),
            created: 1,
        };
        assert_eq!(draft_posts_as(&draft, &p), Verdict::Approve);
        let old = Draft {
            head_sha: "aaaaaaa1111111".into(),
            ..draft.clone()
        };
        assert_eq!(draft_posts_as(&old, &p), Verdict::Comment);
        let unknown = Draft {
            head_sha: String::new(),
            ..draft.clone()
        };
        assert_eq!(draft_posts_as(&unknown, &p), Verdict::Comment);
        // The others say what they say on any commit; the view warns that it's old.
        for v in [Verdict::RequestChanges, Verdict::Comment] {
            assert_eq!(
                draft_posts_as(
                    &Draft {
                        verdict: v,
                        ..old.clone()
                    },
                    &p
                ),
                v
            );
        }
    }

    #[test]
    fn draft_arguments_make_a_draft() {
        let args = parse_draft_args(&strings(&[
            "o/r#7",
            "--verdict",
            "approve",
            "--body-file",
            "x.md",
            "--agent",
            "Claude",
        ]))
        .unwrap();
        let d = args.draft("Looks good.".into(), "d0e6c32aaaa".into(), 42);
        assert_eq!(
            d,
            Draft {
                repo: "o/r".into(),
                number: 7,
                verdict: Verdict::Approve,
                body: "Looks good.".into(),
                author: "Claude".into(),
                head_sha: "d0e6c32aaaa".into(),
                created: 42
            }
        );
        let anonymous = parse_draft_args(&strings(&[
            "o/r#7",
            "--verdict",
            "comment",
            "--body-file",
            "-",
        ]))
        .unwrap();
        assert_eq!(anonymous.draft(String::new(), String::new(), 0).author, "");
    }

    #[test]
    fn an_approval_draft_needs_the_commit_the_agent_read() {
        let parse = |args: &[&str]| parse_draft_args(&strings(args)).unwrap();
        let later = || Ok::<_, String>("f".repeat(40));
        let approve = parse(&["o/r#7", "--verdict", "approve", "--body-file", "-"]);
        // The head at save time may be a push the agent never saw.
        let err = approve.reviewed_head(later).unwrap_err();
        assert!(err.starts_with("an approval needs --head <sha>"), "{err}");
        let pinned = parse(&[
            "o/r#7",
            "--verdict",
            "approve",
            "--body-file",
            "-",
            "--head",
            SHA,
        ]);
        assert_eq!(pinned.reviewed_head(later), Ok(SHA.to_string()));
        // A comment only uses the head for the "new commits" hint.
        let comment = parse(&["o/r#7", "--verdict", "comment", "--body-file", "-"]);
        assert_eq!(comment.reviewed_head(later), Ok("f".repeat(40)));
    }

    #[test]
    fn draft_arguments_are_parsed() {
        let parse = |args: &[&str]| parse_draft_args_on(&strings(args), "github.com");
        let pr = ItemRef {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            kind: ItemKind::Pull,
        };
        assert_eq!(
            parse(&[
                "ArakunHQ/arakun#41",
                "--verdict",
                "request-changes",
                "--body-file",
                "/tmp/review.md"
            ]),
            Ok(DraftArgs {
                item: pr.clone(),
                verdict: Verdict::RequestChanges,
                body_file: "/tmp/review.md".into(),
                agent: None,
                head: None
            })
        );
        // Any order, `--flag=value`, the address instead of the short form, stdin.
        assert_eq!(
            parse(&[
                "--body-file=-",
                "--agent",
                "codex",
                "--verdict=approve",
                &format!("--head={}", SHA.to_uppercase()),
                "https://github.com/ArakunHQ/arakun/pull/41"
            ]),
            Ok(DraftArgs {
                item: pr,
                verdict: Verdict::Approve,
                body_file: "-".into(),
                agent: Some("codex".into()),
                head: Some(SHA.into())
            })
        );
        assert_eq!(
            parse(&["o/r#1", "--verdict", "comment", "--body-file", "a=b.md"])
                .unwrap()
                .body_file,
            PathBuf::from("a=b.md")
        );

        let err = |args: &[&str]| parse(args).unwrap_err();
        assert!(err(&[]).starts_with("missing the pull request"));
        assert!(err(&["o/r#1", "--body-file", "f"]).starts_with("missing --verdict"));
        assert!(err(&["o/r#1", "--verdict", "approve"]).starts_with("missing --body-file"));
        assert!(
            err(&["o/r#1", "--verdict", "lgtm", "--body-file", "f"])
                .starts_with("`lgtm` is not a verdict")
        );
        assert!(err(&["o/r#1", "--verdict"]).starts_with("`--verdict` needs a value"));
        assert!(err(&["o/r#1", "--verdict="]).starts_with("`--verdict` needs a value"));
        assert!(err(&["o/r#1", "--post"]).starts_with("unknown option `--post`"));
        assert!(err(&["o/r#1", "--head", "d0e6c32"]).contains("not a full commit SHA"));
        assert!(err(&["o/r#1", "--head", "--agent"]).contains("not a full commit SHA"));
        assert!(err(&["o/r#1", "o/r#2"]).starts_with("unexpected argument `o/r#2`"));
        assert!(err(&["arakun 41"]).starts_with("`arakun 41` is not a pull request"));
        assert!(
            err(&[
                "https://github.com/o/r/issues/3",
                "--verdict",
                "comment",
                "--body-file",
                "f"
            ])
            .contains("is an issue")
        );
        {
            assert!(
                err(&[
                    "https://ghe.corp/o/r/pull/7",
                    "--verdict",
                    "comment",
                    "--body-file",
                    "f"
                ])
                .contains("is not on github.com")
            );
        }
        assert!(err(&[]).contains(DRAFT_USAGE));
        for v in [Verdict::Approve, Verdict::RequestChanges, Verdict::Comment] {
            assert_eq!(Verdict::from_name(v.name()), Some(v));
        }
    }
}
