//! skills.sh, the open directory of agent skills: its search, and installing a
//! skill from the GitHub repository it lists.
//!
//! The search is the public one the `skills` CLI uses (`/api/search`); skills.sh's
//! own API wants a Vercel token a desktop app does not have. A skill is a folder
//! with a SKILL.md, so installing one copies that folder from GitHub into
//! `~/.claude/skills`, where Claude Code and Workers AI both read it. Nothing from
//! npm runs. A repository with several skills is a package: [`package`] reads
//! them all, so they install in one go.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

use super::launch;

const SEARCH: &str = "https://skills.sh/api/search";
/// Results one search asks for.
const LIMIT: usize = 100;
/// A skill this big is something else; it is not copied.
const MAX_FILES: usize = 400;
const MAX_BYTES: u64 = 25 << 20;
/// SKILL.md files read to match a skill by its front matter, when no folder has its name.
const MAX_PROBES: usize = 25;
/// Skills one package installs at most.
const MAX_PACKAGE: usize = 100;
/// A SKILL.md bigger than this is not read for its name and description.
const MAX_SKILL_MD: u64 = 1 << 20;
/// Files read from GitHub at once: one after another is slow for a big skill or package.
const READERS: usize = 8;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillEntry {
    /// `owner/repo/skill`.
    pub id: String,
    /// The GitHub repository, `owner/repo`.
    pub source: String,
    #[serde(rename = "skillId")]
    pub skill_id: String,
    pub name: String,
    #[serde(default)]
    pub installs: u64,
    /// What its SKILL.md says, when it was read with its package; skills.sh lists none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl SkillEntry {
    pub fn owner(&self) -> &str {
        self.source.split('/').next().unwrap_or_default()
    }

    pub fn page(&self) -> String {
        format!("https://skills.sh/{}", self.id)
    }
}

/// Blocking: the skills matching `query`, most installed first. skills.sh needs
/// two characters at least; fewer find nothing.
pub fn search(query: &str) -> Result<Vec<SkillEntry>> {
    let q = query.trim();
    if q.chars().count() < 2 {
        return Ok(Vec::new());
    }
    let url = format!("{SEARCH}?q={}&limit={LIMIT}", encode(q));
    let body = curl(&url, &[]).map_err(|e| {
        if format!("{e:#}").contains("429") {
            anyhow!(
                "skills.sh is getting too many searches from here. Wait a minute and search again."
            )
        } else {
            e.context("could not reach skills.sh")
        }
    })?;
    let v: Value =
        serde_json::from_slice(&body).context("skills.sh answered something that is not JSON")?;
    parse_search(&v)
}

fn parse_search(v: &Value) -> Result<Vec<SkillEntry>> {
    if let Some(e) = v["error"].as_str() {
        bail!("skills.sh: {e}");
    }
    let mut list: Vec<SkillEntry> = v["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| serde_json::from_value(s.clone()).ok())
        .filter(|s: &SkillEntry| s.source.split('/').count() == 2 && !s.skill_id.is_empty())
        .collect();
    list.sort_by_key(|s| std::cmp::Reverse(s.installs));
    list.dedup_by(|a, b| a.id == b.id);
    Ok(list)
}

// ---------------------------------------------------------------------------
// The skill on GitHub.

/// Where a skill lives in its repository, and what it says about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillSource {
    pub source: String,
    /// The commit everything was read at, so Install copies what the details showed.
    pub rev: String,
    /// The folder holding SKILL.md, `""` for the repository's root.
    pub dir: String,
    pub description: String,
    /// Paths under `dir` and their sizes.
    pub files: Vec<(String, u64)>,
    /// The paths of `files` that are executable.
    executable: Vec<String>,
    /// The skill ids of every skill in the repository, this one too.
    pub package: Vec<String>,
}

impl SkillSource {
    pub fn url(&self) -> String {
        if self.dir.is_empty() {
            format!("https://github.com/{}/tree/{}", self.source, self.rev)
        } else {
            format!(
                "https://github.com/{}/tree/{}/{}",
                self.source, self.rev, self.dir
            )
        }
    }

    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|(_, n)| n).sum()
    }
}

/// A file of the repository's tree.
#[derive(Debug, PartialEq)]
struct Blob {
    path: String,
    size: u64,
    executable: bool,
}

/// Blocking, a request or a few: finds the skill's folder and reads its SKILL.md.
pub fn locate(entry: &SkillEntry) -> Result<SkillSource> {
    let (rev, blobs) = read_tree(&entry.source)?;
    let dir = match pick_dir(&blobs, &entry.skill_id, &repo_name(&entry.source)) {
        Some(d) => d,
        None => probe(entry, &rev, &blobs)?,
    };
    let text =
        String::from_utf8_lossy(&raw(&entry.source, &rev, &join(&dir, "SKILL.md"))?).into_owned();
    let (_, description) = super::claude_ext::parse_skill_frontmatter(&text);
    let ids = package_dirs(&blobs, &repo_name(&entry.source))
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    Ok(source_at(
        &entry.source,
        &rev,
        &blobs,
        dir,
        description,
        ids,
    ))
}

/// The commit the repository is at, and its files then.
fn read_tree(source: &str) -> Result<(String, Vec<Blob>)> {
    let rev = head_commit(source)?;
    let tree = github_json(&format!("repos/{source}/git/trees/{rev}?recursive=1"))?;
    // A cut list could leave out files the skill needs.
    if tree["truncated"].as_bool() == Some(true) {
        bail!("{source} has too many files for GitHub to list at once");
    }
    Ok((rev, blobs(&tree)))
}

/// The skill in `dir`, from the repository's files.
fn source_at(
    source: &str,
    rev: &str,
    blobs: &[Blob],
    dir: String,
    description: String,
    package: Vec<String>,
) -> SkillSource {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let inside: Vec<&Blob> = blobs
        .iter()
        .filter(|b| b.path.starts_with(&prefix))
        .collect();
    SkillSource {
        source: source.to_string(),
        rev: rev.to_string(),
        description,
        files: inside
            .iter()
            .map(|b| (b.path[prefix.len()..].to_string(), b.size))
            .collect(),
        executable: inside
            .iter()
            .filter(|b| b.executable)
            .map(|b| b.path[prefix.len()..].to_string())
            .collect(),
        dir,
        package,
    }
}

// ---------------------------------------------------------------------------
// Packages: a repository of skills.

/// Every skill in a repository, read at one commit.
#[derive(Clone, Debug)]
pub struct Package {
    /// `owner/repo`.
    pub source: String,
    pub skills: Vec<(SkillEntry, SkillSource)>,
}

/// The GitHub repository a search names, `owner/repo`: typed as is, or as a link
/// to it (`https://github.com/owner/repo/tree/main/skills`).
pub fn repo_of(query: &str) -> Option<String> {
    let q = query.trim();
    if q.contains(char::is_whitespace) {
        return None;
    }
    let link = ["https://", "http://", ""]
        .iter()
        .find_map(|p| {
            let q = q.strip_prefix(p)?;
            q.strip_prefix("www.")
                .unwrap_or(q)
                .strip_prefix("github.com/")
        })
        // A link copied from the browser may end in `?tab=readme` or `#readme`.
        .map(|l| l.split(['?', '#']).next().unwrap_or_default());
    let rest = link.unwrap_or(q);
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    // Plain `a/b/c` is a skills.sh id, not a repository.
    if link.is_none() && parts.next().is_some() {
        return None;
    }
    let plain = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (plain(owner) && plain(repo)).then(|| format!("{owner}/{repo}"))
}

/// The skills of a repository by the id each installs as (its folder's name, the
/// repository's for one at the root), with their folders. Of two folders with one
/// name, the shallower; a name no folder can have is left out.
fn package_dirs(blobs: &[Blob], repo: &str) -> Vec<(String, String)> {
    let mut dirs = skill_dirs(blobs);
    dirs.sort_by_key(|d| (d.matches('/').count(), d.len(), d.to_string()));
    let mut out: Vec<(String, String)> = Vec::new();
    for dir in dirs {
        let id = if dir.is_empty() {
            repo
        } else {
            dir.rsplit('/').next().unwrap_or_default()
        };
        if folder_name(id).is_err() || out.iter().any(|(i, _)| i.eq_ignore_ascii_case(id)) {
            continue;
        }
        out.push((id.to_string(), dir.to_string()));
    }
    out.sort_by_key(|(id, _)| id.to_lowercase());
    out
}

/// Blocking: every skill in the repository `source`, with what its SKILL.md says.
pub fn package(source: &str) -> Result<Package> {
    let (rev, blobs) = read_tree(source)?;
    let dirs = package_dirs(&blobs, &repo_name(source));
    if dirs.is_empty() {
        bail!("{source} has no SKILL.md");
    }
    if dirs.len() > MAX_PACKAGE {
        bail!(
            "{source} has {} skills, more than the {MAX_PACKAGE} a package installs",
            dirs.len()
        );
    }
    let ids: Vec<String> = dirs.iter().map(|(id, _)| id.clone()).collect();
    let paths: Vec<String> = dirs.iter().map(|(_, d)| join(d, "SKILL.md")).collect();
    // Only the front matter is kept: each chunk is parsed as it comes, and a
    // SKILL.md too big to be one is not read here (it still installs).
    let mut heads = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(READERS) {
        let small: Vec<String> = chunk
            .iter()
            .filter(|p| blob_size(&blobs, p) <= MAX_SKILL_MD)
            .cloned()
            .collect();
        let mut got = raw_many(source, &rev, &small).into_iter();
        for p in chunk {
            let text = if blob_size(&blobs, p) <= MAX_SKILL_MD {
                got.next().and_then(|r| r.ok())
            } else {
                None
            };
            heads.push(
                text.map(|b| {
                    super::claude_ext::parse_skill_frontmatter(&String::from_utf8_lossy(&b))
                })
                .unwrap_or_default(),
            );
        }
    }
    let skills = dirs
        .into_iter()
        .zip(heads)
        .map(|((id, dir), (name, description))| {
            let entry = SkillEntry {
                id: format!("{source}/{id}"),
                source: source.to_string(),
                skill_id: id.clone(),
                name: name.unwrap_or(id),
                installs: 0,
                description: description.clone(),
            };
            let found = source_at(source, &rev, &blobs, dir, description, ids.clone());
            (entry, found)
        })
        .collect();
    Ok(Package {
        source: source.to_string(),
        skills,
    })
}

/// The size the tree gives a file, 0 for one it does not list.
fn blob_size(blobs: &[Blob], path: &str) -> u64 {
    blobs.iter().find(|b| b.path == path).map_or(0, |b| b.size)
}

fn repo_name(source: &str) -> String {
    source.rsplit('/').next().unwrap_or_default().to_string()
}

fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// The files of a `git/trees` answer; links and submodules are left out.
fn blobs(tree: &Value) -> Vec<Blob> {
    tree["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["type"] == "blob")
        .filter_map(|e| {
            let mode = e["mode"].as_str()?;
            if mode == "120000" {
                return None;
            }
            Some(Blob {
                path: e["path"].as_str()?.to_string(),
                size: e["size"].as_u64().unwrap_or(0),
                executable: mode == "100755",
            })
        })
        .collect()
}

/// The folders with a SKILL.md, `""` for the root.
fn skill_dirs(blobs: &[Blob]) -> Vec<&str> {
    blobs
        .iter()
        .filter_map(|b| {
            if b.path == "SKILL.md" {
                Some("")
            } else {
                b.path.strip_suffix("/SKILL.md")
            }
        })
        .collect()
}

/// The folder named as the skill; of several, the shallowest. A repository that is
/// one skill has it at the root. A folder named otherwise is only taken when its
/// SKILL.md says it is the skill (`probe`): a lone folder may be another skill.
fn pick_dir(blobs: &[Blob], skill: &str, repo: &str) -> Option<String> {
    let dirs = skill_dirs(blobs);
    let named = |d: &&str| {
        let base = d.rsplit('/').next().unwrap_or_default();
        base.eq_ignore_ascii_case(skill) || (d.is_empty() && repo.eq_ignore_ascii_case(skill))
    };
    let mut hits: Vec<&str> = dirs.iter().copied().filter(named).collect();
    hits.sort_by_key(|d| (d.matches('/').count(), d.len()));
    hits.first().map(|d| d.to_string())
}

/// The folder whose SKILL.md names the skill, for folders named otherwise.
fn probe(entry: &SkillEntry, rev: &str, blobs: &[Blob]) -> Result<String> {
    let dirs = skill_dirs(blobs);
    if dirs.is_empty() {
        bail!("{} has no SKILL.md", entry.source);
    }
    for dir in dirs.iter().take(MAX_PROBES) {
        let Ok(bytes) = raw(&entry.source, rev, &join(dir, "SKILL.md")) else {
            continue;
        };
        let (name, _) =
            super::claude_ext::parse_skill_frontmatter(&String::from_utf8_lossy(&bytes));
        if name.is_some_and(|n| n.eq_ignore_ascii_case(&entry.skill_id)) {
            return Ok(dir.to_string());
        }
    }
    bail!("could not find {} in {}", entry.skill_id, entry.source)
}

#[cfg(test)]
pub(crate) fn test_source(dir: &str, files: &[(&str, u64)]) -> SkillSource {
    SkillSource {
        source: "o/r".into(),
        rev: "0".repeat(40),
        dir: dir.into(),
        description: String::new(),
        files: files.iter().map(|(p, n)| (p.to_string(), *n)).collect(),
        executable: Vec::new(),
        package: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Installing.

/// Where personal skills go: Claude Code reads them, and so does Workers AI.
pub fn skills_root() -> PathBuf {
    launch::home().join(".claude").join("skills")
}

/// Blocking: copies the skill's folder into `root/<skill>`. A folder already there
/// is left alone, and a half-copied one is never where agents look for skills.
pub fn install(entry: &SkillEntry, found: &SkillSource, root: &Path) -> Result<PathBuf> {
    let name = folder_name(&entry.skill_id)?;
    let dest = root.join(name);
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    check_size(found)?;
    std::fs::create_dir_all(root).with_context(|| format!("cannot create {}", root.display()))?;
    // Beside the skills folder, not in it: agents list every folder there with a
    // SKILL.md. The same file system keeps the final move a rename.
    let staging = root
        .parent()
        .unwrap_or(root)
        .join(format!(".arakun-skill-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| -> Result<()> {
        let mut received = 0u64;
        for chunk in found.files.chunks(READERS) {
            // Every path is checked before anything of the chunk is read.
            let rels = chunk
                .iter()
                .map(|(p, _)| safe_relative(p))
                .collect::<Result<Vec<_>>>()?;
            let paths: Vec<String> = chunk.iter().map(|(p, _)| join(&found.dir, p)).collect();
            let got = raw_many(&found.source, &found.rev, &paths);
            for (((path, _), rel), bytes) in chunk.iter().zip(rels).zip(got) {
                let to = staging.join(rel);
                write_file(&to, bytes?, &mut received)?;
                #[cfg(unix)]
                if found.executable.iter().any(|e| e == path) {
                    use std::os::unix::fs::PermissionsExt as _;
                    let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755));
                }
                #[cfg(not(unix))]
                let _ = path;
            }
        }
        if !staging.join("SKILL.md").is_file() {
            bail!("the copy has no SKILL.md");
        }
        publish(&staging, &dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map(|_| dest)
}

/// Writes one file of the skill to `to`, counting what came so far.
fn write_file(to: &Path, bytes: Vec<u8>, received: &mut u64) -> Result<()> {
    // The sizes were read before; a file may have grown since.
    *received += bytes.len() as u64;
    if *received > MAX_BYTES {
        bail!(
            "it is more than the {} MB a skill is copied with",
            MAX_BYTES >> 20
        );
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    write_new(to, &bytes)
}

/// Writes a file that must not be there yet: two paths that name one file (`A.md`
/// and `a.md` where case does not count) stop the copy instead of overwriting.
fn write_new(to: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .with_context(|| format!("cannot write {}", to.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("cannot write {}", to.display()))
}

/// Moves the finished copy to `dest`, never over a folder someone made there.
fn publish(staging: &Path, dest: &Path) -> Result<()> {
    // On Unix a rename replaces an empty folder, so `dest` is claimed first: making
    // it fails if it is there, and only this empty folder can then be replaced.
    #[cfg(unix)]
    if let Err(e) = std::fs::create_dir(dest) {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            bail!("{} already exists", dest.display());
        }
        return Err(e).with_context(|| format!("cannot create {}", dest.display()));
    }
    // Elsewhere a rename onto a folder that is there fails by itself.
    std::fs::rename(staging, dest).map_err(|e| {
        #[cfg(unix)]
        let _ = std::fs::remove_dir(dest);
        anyhow!(e).context(format!("cannot move it to {}", dest.display()))
    })
}

fn check_size(found: &SkillSource) -> Result<()> {
    if found.files.len() > MAX_FILES {
        bail!(
            "it has {} files, more than the {MAX_FILES} a skill is copied with",
            found.files.len()
        );
    }
    if found.bytes() > MAX_BYTES {
        bail!(
            "it is {} MB, more than the {} MB a skill is copied with",
            found.bytes() >> 20,
            MAX_BYTES >> 20
        );
    }
    Ok(())
}

/// The skill's folder name: one plain path component.
pub(super) fn folder_name(skill: &str) -> Result<&str> {
    let ok = !skill.is_empty()
        && !skill.starts_with('.')
        && skill
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(skill)
    } else {
        Err(anyhow!("{skill:?} is not a name a skill folder can have"))
    }
}

/// A repository path that stays inside the folder it is copied to.
fn safe_relative(path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    if path.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("{path:?} is not a path inside the skill");
    }
    Ok(p.to_path_buf())
}

// ---------------------------------------------------------------------------
// Fetching.

/// The commit the repository's default branch is at.
fn head_commit(source: &str) -> Result<String> {
    let sha = github_get(
        &format!("repos/{source}/commits/HEAD"),
        "Accept: application/vnd.github.sha",
    )?;
    let sha = String::from_utf8_lossy(&sha).trim().to_string();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("GitHub did not say which commit {source} is at");
    }
    Ok(sha)
}

fn github_json(path: &str) -> Result<Value> {
    let body = github_get(path, "Accept: application/vnd.github+json")?;
    let v: Value =
        serde_json::from_slice(&body).context("GitHub answered something that is not JSON")?;
    if let Some(m) = v["message"].as_str() {
        bail!("GitHub: {m}");
    }
    Ok(v)
}

/// The GitHub API with `gh`'s token when it has one (5,000 calls an hour), else
/// without (60).
fn github_get(path: &str, accept: &str) -> Result<Vec<u8>> {
    let url = format!("https://api.github.com/{path}");
    match gh_token() {
        Some(token) => curl_with(
            &url,
            &[accept],
            Some(&format!("Authorization: Bearer {token}")),
        ),
        None => curl(&url, &[accept]),
    }
    .map_err(|e| {
        let e = format!("{e:#}");
        if e.contains("403") || e.contains("429") {
            anyhow!(
                "GitHub limits how often it is asked without signing in. Sign in with `gh auth login`, or try again in an hour."
            )
        } else {
            anyhow!("GitHub: {e}")
        }
    })
}

/// `GH_TOKEN`, `GITHUB_TOKEN`, or the token `gh` is signed in with.
fn gh_token() -> Option<String> {
    let env = ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    env.or_else(|| {
        let out = crate::platform::command("gh")
            .args(["auth", "token", "--hostname", "github.com"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && !token.is_empty()).then_some(token)
    })
    .map(|t| t.trim().to_string())
}

/// A file of the repository at commit `rev`.
fn raw(source: &str, rev: &str, path: &str) -> Result<Vec<u8>> {
    let path: String = path.split('/').map(encode).collect::<Vec<_>>().join("/");
    curl(
        &format!("https://raw.githubusercontent.com/{source}/{rev}/{path}"),
        &[],
    )
}

/// Files of the repository at commit `rev`, read side by side, in order.
fn raw_many(source: &str, rev: &str, paths: &[String]) -> Vec<Result<Vec<u8>>> {
    std::thread::scope(|s| {
        let reads: Vec<_> = paths
            .iter()
            .map(|p| s.spawn(move || raw(source, rev, p)))
            .collect();
        reads
            .into_iter()
            .map(|r| {
                r.join()
                    .unwrap_or_else(|_| Err(anyhow!("reading it stopped")))
            })
            .collect()
    })
}

fn curl(url: &str, headers: &[&str]) -> Result<Vec<u8>> {
    curl_with(url, headers, None)
}

/// `secret` is a header curl reads from its stdin, so no other process sees it
/// in curl's command line.
fn curl_with(url: &str, headers: &[&str], secret: Option<&str>) -> Result<Vec<u8>> {
    use std::io::Write as _;
    let mut cmd = crate::platform::command("curl");
    // `--disable` first: a curlrc that turns on a trace would write the token to it.
    cmd.args([
        "--disable",
        "-sSfL",
        "-m",
        "30",
        "--max-filesize",
        "26214400",
    ]);
    for h in headers {
        cmd.args(["-H", h]);
    }
    if secret.is_some() {
        cmd.args(["-H", "@-"]);
    }
    let mut child = cmd
        .arg(url)
        .stdin(if secret.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl is needed to reach skills.sh and GitHub")?;
    if let (Some(secret), Some(mut stdin)) = (secret, child.stdin.take()) {
        let _ = writeln!(stdin, "{secret}");
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree(paths: &[(&str, &str)]) -> Vec<Blob> {
        let v = json!({"tree": paths.iter().map(|(p, mode)| json!({
            "path": p, "mode": mode, "type": if *mode == "040000" { "tree" } else { "blob" }, "size": 10,
        })).collect::<Vec<_>>()});
        blobs(&v)
    }

    #[test]
    fn search_results_sort_by_installs_and_drop_odd_ones() {
        let v = json!({"skills": [
            {"id": "a/b/x", "source": "a/b", "skillId": "x", "name": "x", "installs": 5},
            {"id": "a/b/y", "source": "a/b", "skillId": "y", "name": "y", "installs": 50},
            {"id": "bad", "source": "nobody", "skillId": "z", "name": "z"},
        ]});
        let list = parse_search(&v).unwrap();
        assert_eq!(
            list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["y", "x"]
        );
        assert_eq!(list[0].owner(), "a");
        assert_eq!(list[0].page(), "https://skills.sh/a/b/y");
        assert!(parse_search(&json!({"error": "Query must be at least 2 characters"})).is_err());
        assert!(search("a").unwrap().is_empty());
    }

    #[test]
    fn the_skill_folder_is_the_one_named_as_the_skill() {
        let blobs = tree(&[
            ("README.md", "100644"),
            ("skills/pdf/SKILL.md", "100644"),
            ("skills/pdf/scripts/fill.py", "100755"),
            ("skills/pdf/link", "120000"),
            ("archive/old/pdf/SKILL.md", "100644"),
            ("skills/docx/SKILL.md", "100644"),
            ("skills", "040000"),
        ]);
        assert_eq!(blobs.len(), 5);
        assert!(blobs.iter().any(|b| b.executable));
        assert_eq!(
            pick_dir(&blobs, "pdf", "skills").as_deref(),
            Some("skills/pdf")
        );
        assert_eq!(
            pick_dir(&blobs, "PDF", "skills").as_deref(),
            Some("skills/pdf")
        );
        assert_eq!(pick_dir(&blobs, "xlsx", "skills"), None);

        let root = tree(&[("SKILL.md", "100644"), ("ref.md", "100644")]);
        assert_eq!(pick_dir(&root, "my-skill", "my-skill").as_deref(), Some(""));
        // A lone folder named otherwise may be another skill: `probe` reads its name.
        let single = tree(&[("src/thing/SKILL.md", "100644")]);
        assert_eq!(pick_dir(&single, "other-name", "repo"), None);
    }

    #[test]
    fn only_plain_names_and_paths_are_written() {
        assert!(folder_name("pdf").is_ok());
        assert!(folder_name("my_skill.v2").is_ok());
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "x y"] {
            assert!(folder_name(bad).is_err(), "{bad}");
        }
        assert!(safe_relative("scripts/run.sh").is_ok());
        for bad in ["", "../x", "/etc/passwd", "a/../../b", "./a"] {
            assert!(safe_relative(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_install_never_replaces_a_folder_or_copies_too_much() {
        let root = std::env::temp_dir().join(format!("arakun-skills-sh-{}", std::process::id()));
        std::fs::create_dir_all(root.join("pdf")).unwrap();
        let entry = SkillEntry {
            id: "a/b/pdf".into(),
            source: "a/b".into(),
            skill_id: "pdf".into(),
            name: "pdf".into(),
            installs: 0,
            description: String::new(),
        };
        let mut found = SkillSource {
            source: "a/b".into(),
            rev: "0".repeat(40),
            dir: "pdf".into(),
            description: String::new(),
            files: vec![("SKILL.md".into(), 10)],
            executable: Vec::new(),
            package: Vec::new(),
        };
        let e = install(&entry, &found, &root).unwrap_err();
        assert!(e.to_string().contains("already exists"));

        let other = SkillEntry {
            skill_id: "big".into(),
            ..entry
        };
        found.files = vec![("huge.bin".into(), MAX_BYTES + 1)];
        let e = install(&other, &found, &root).unwrap_err();
        assert!(e.to_string().contains("MB"), "{e}");
        assert!(!root.join("big").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_that_name_one_file_and_folders_made_meanwhile_stop_the_copy() {
        let dir = std::env::temp_dir().join(format!("arakun-skills-pub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_new(&dir.join("a.md"), b"one").unwrap();
        assert!(write_new(&dir.join("a.md"), b"two").is_err());
        assert_eq!(std::fs::read(dir.join("a.md")).unwrap(), b"one");

        let staging = dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("SKILL.md"), "x").unwrap();
        // Someone made the folder after the first look: it stays as it was.
        let dest = dir.join("taken");
        std::fs::create_dir(&dest).unwrap();
        let e = publish(&staging, &dest).unwrap_err();
        #[cfg(unix)]
        assert!(format!("{e:#}").contains("already exists"), "{e:#}");
        let _ = e;
        assert!(staging.join("SKILL.md").is_file());
        assert!(!dest.join("SKILL.md").exists());

        publish(&staging, &dir.join("free")).unwrap();
        assert!(dir.join("free/SKILL.md").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_search_can_name_a_repository() {
        for q in [
            "anthropics/skills",
            " anthropics/skills ",
            "github.com/anthropics/skills",
            "https://github.com/anthropics/skills",
            "https://github.com/anthropics/skills.git",
            "https://github.com/anthropics/skills/tree/main/skills/pdf",
            "https://www.github.com/anthropics/skills",
            "https://github.com/anthropics/skills?tab=readme-ov-file",
            "https://github.com/anthropics/skills#readme",
        ] {
            assert_eq!(repo_of(q).as_deref(), Some("anthropics/skills"), "{q}");
        }
        for q in [
            "pdf",
            "pdf tools",
            "anthropics/skills/pdf",
            "a/",
            "/b",
            "../b",
            "a/b c",
            "https://gitlab.com/a/b",
        ] {
            assert_eq!(repo_of(q), None, "{q}");
        }
    }

    #[test]
    fn a_package_is_every_skill_folder_once() {
        let blobs = tree(&[
            ("SKILL.md", "100644"),
            ("skills/pdf/SKILL.md", "100644"),
            ("skills/pdf/fill.py", "100755"),
            ("archive/old/pdf/SKILL.md", "100644"),
            ("skills/Docx/SKILL.md", "100644"),
            ("skills/bad name/SKILL.md", "100644"),
            ("README.md", "100644"),
        ]);
        let got = package_dirs(&blobs, "toolkit");
        assert_eq!(
            got,
            [
                ("Docx".to_string(), "skills/Docx".to_string()),
                ("pdf".into(), "skills/pdf".into()),
                ("toolkit".into(), "".into()),
            ]
        );
        // Each installs from where `locate` would find it again on a server.
        for (id, dir) in &got {
            assert_eq!(pick_dir(&blobs, id, "toolkit").as_ref(), Some(dir));
        }
        let pdf = source_at(
            "o/toolkit",
            "r",
            &blobs,
            "skills/pdf".into(),
            String::new(),
            vec![],
        );
        assert_eq!(pdf.files.len(), 2);
        assert_eq!(pdf.executable, ["fill.py"]);
    }

    #[test]
    fn source_links_point_at_the_folder() {
        let rev = "a".repeat(40);
        let mut s = SkillSource {
            source: "anthropics/skills".into(),
            rev: rev.clone(),
            dir: "skills/pdf".into(),
            description: String::new(),
            files: vec![("SKILL.md".into(), 3), ("a.py".into(), 4)],
            executable: Vec::new(),
            package: Vec::new(),
        };
        assert_eq!(
            s.url(),
            format!("https://github.com/anthropics/skills/tree/{rev}/skills/pdf")
        );
        assert_eq!(s.bytes(), 7);
        s.dir.clear();
        assert_eq!(
            s.url(),
            format!("https://github.com/anthropics/skills/tree/{rev}")
        );
    }

    #[test]
    #[ignore = "reaches skills.sh and GitHub"]
    fn skills_sh_live() {
        let list = search("pdf").unwrap();
        let pdf = list
            .iter()
            .find(|s| s.id == "anthropics/skills/pdf")
            .unwrap();
        let found = locate(pdf).unwrap();
        eprintln!("{} {} {:?}", found.dir, found.description, found.files);
        assert!(found.files.iter().any(|(p, _)| p == "SKILL.md"));
        let root = std::env::temp_dir().join(format!("arakun-skills-live-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let dest = install(pdf, &found, &root).unwrap();
        assert!(dest.join("SKILL.md").is_file());
        assert!(found.package.iter().any(|id| id == "docx"));
        let _ = std::fs::remove_dir_all(&root);
        let pkg = package("anthropics/skills").unwrap();
        let (_, docx) = pkg
            .skills
            .iter()
            .find(|(e, _)| e.skill_id == "docx")
            .unwrap();
        assert!(!docx.description.is_empty());
    }
}
