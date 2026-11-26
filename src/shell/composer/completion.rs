//! Completion in the composer: files after `@`, agent commands and skills
//! after `/`, saved prompts after `!`.

use super::*;

/// Rows the composer completion popup shows at most.
const COMPLETION_ROWS: usize = 8;
/// `git ls-files` is re-read at most this often while completing.
const FILE_INDEX_TTL: Duration = Duration::from_secs(30);

/// The composer completion popup: files after `@`, agent commands after `/`.
pub(in crate::shell) struct Completion {
    /// Byte range in the draft of the token being completed (from the `@`/`/`
    /// to the caret); accepting an item replaces it.
    token_start: usize,
    token_end: usize,
    pub(super) items: Vec<CompletionItem>,
    pub(super) selected: usize,
    /// Saved prompts (`!`), which can be pinned and deleted from the list.
    pub(super) prompts: bool,
}

pub(in crate::shell) struct CompletionItem {
    pub(super) label: SharedString,
    pub(super) detail: Option<SharedString>,
    /// Replaces the token, trailing space included.
    insert: String,
    /// The saved prompt this inserts.
    prompt: Option<usize>,
}

impl Completion {
    /// The saved prompt the selected row inserts, in the `!` list.
    pub(super) fn selected_prompt(&self) -> Option<usize> {
        self.prompts
            .then(|| self.items.get(self.selected)?.prompt)
            .flatten()
    }

    /// Down (`forward`) or up a row, round the ends.
    pub(super) fn step(&mut self, forward: bool) {
        let n = self.items.len().max(1);
        self.selected = if forward {
            (self.selected + 1) % n
        } else {
            self.selected.checked_sub(1).unwrap_or(n - 1)
        };
    }

    pub(super) fn select(&mut self, i: usize) {
        self.selected = i.min(self.items.len().saturating_sub(1));
    }
}

impl Composer {
    /// The files of the visible environment for `@` completion, loaded in the
    /// background on the first ask (or once stale).
    fn file_index(&mut self, cx: &mut Context<Self>) -> Option<Arc<Vec<String>>> {
        let env_path = self.frame.env.clone();
        if let Some((path, at, files)) = &self.file_index
            && *path == env_path
            && at.elapsed() < FILE_INDEX_TTL
        {
            return Some(files.clone());
        }
        if self.file_index_loading.as_ref() == Some(&env_path) {
            return None;
        }
        self.file_index_loading = Some(env_path.clone());
        // A server's environment: its files are listed over there, and
        // the answer fills the index (`server_files_message`).
        if crate::model::Remote::is_path(&env_path) {
            cx.emit(ComposerEvent::ListFiles(env_path));
            return None;
        }
        cx.spawn(async move |this, cx| {
            let p = env_path.clone();
            let files = cx
                .background_spawn(crate::guard::task(async move {
                    git::ls_files(&p).unwrap_or_default()
                }))
                .await;
            let _ = this.update(cx, |this, cx| this.set_file_index(env_path, files, cx));
        })
        .detach();
        None
    }

    /// The files of `env` arrived.
    pub fn set_file_index(&mut self, env: PathBuf, files: Vec<String>, cx: &mut Context<Self>) {
        if self.file_index_loading.as_ref() == Some(&env) {
            self.file_index_loading = None;
        }
        self.file_index = Some((env, Instant::now(), Arc::new(files)));
        // The popup may have been waiting for them.
        self.update_completion(cx);
    }

    /// The files of `env` could not be listed; the next `@` asks again.
    pub fn file_index_failed(&mut self, env: &Path) {
        if self.file_index_loading.as_deref() == Some(env) {
            self.file_index_loading = None;
        }
    }

    /// The skills of the visible environment for `/` completion, read in the
    /// background on the first ask (or once stale). Claude Code and Workers AI
    /// read them, so they show before a session has said what it offers.
    /// With `mirrored`, for a server's environment, only what this computer
    /// mirrors to servers: the rest is not there.
    fn skill_index(
        &mut self,
        mirrored: bool,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Vec<crate::agent::Choice>>> {
        let env_path = self.frame.env.clone();
        if let Some((path, at, skills)) = &self.skill_index
            && *path == env_path
            && at.elapsed() < FILE_INDEX_TTL
        {
            return Some(skills.clone());
        }
        if !self.skill_index_loading {
            self.skill_index_loading = true;
            let for_path = env_path.clone();
            cx.spawn(async move |this, cx| {
                let p = for_path.clone();
                let skills = cx
                    .background_spawn(crate::guard::task(async move {
                        if mirrored {
                            crate::agent::mirror::mirrored_commands()
                        } else {
                            crate::agent::claude_ext::skill_choices(&p)
                        }
                    }))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.skill_index_loading = false;
                    this.skill_index = Some((for_path, Instant::now(), Arc::new(skills)));
                    this.update_completion(cx);
                });
            })
            .detach();
        }
        // While they are read again, the last ones read for this environment.
        self.skill_index
            .as_ref()
            .filter(|(p, ..)| *p == env_path)
            .map(|(_, _, s)| s.clone())
    }

    pub fn update_completion(&mut self, cx: &mut Context<Self>) {
        let was_open = self.completion.is_some();
        self.completion = self.compute_completion(cx);
        if was_open || self.completion.is_some() {
            cx.notify();
        }
    }

    fn compute_completion(&mut self, cx: &mut Context<Self>) -> Option<Completion> {
        if !self.frame.has_projects {
            return None;
        }
        let draft = self.here().text.bare();
        let token_end = draft.head();
        // `!` starting the draft: saved prompts, the rest of the line filtering them.
        if let Some(query) = draft.strip_prefix('!')
            && !query.contains('\n')
        {
            return Some(self.prompt_completion(query, &draft));
        }
        let (token_start, token) = draft.token()?;
        if token.is_empty() {
            return None;
        }
        // `/cmd` as the only token: the agent's own reported commands.
        if token_start == 0
            && let Some(name) = token.strip_prefix('/')
        {
            if name.contains(char::is_whitespace) {
                return None;
            }
            let items = self.command_items(&name.to_lowercase(), cx);
            return (!items.is_empty()).then_some(Completion {
                token_start,
                token_end,
                items,
                selected: 0,
                prompts: false,
            });
        }
        // `@path` in any token: the environment's files.
        if let Some(prefix) = token.strip_prefix('@') {
            let files = self.file_index(cx)?;
            let items = file_items(&files, &prefix.to_lowercase());
            return (!items.is_empty()).then_some(Completion {
                token_start,
                token_end,
                items,
                selected: 0,
                prompts: false,
            });
        }
        None
    }

    /// The saved prompts matching `query`, for a draft that starts with `!`.
    fn prompt_completion(&self, query: &str, draft: &str) -> Completion {
        let mut items: Vec<CompletionItem> = crate::prompts::ranked(&self.prompts, query)
            .into_iter()
            .take(COMPLETION_ROWS)
            .map(|i| {
                let p = &self.prompts[i];
                let first = p
                    .text
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim();
                CompletionItem {
                    label: format!("{}!{}", if p.pinned { "★ " } else { "" }, p.name).into(),
                    detail: Some(first.to_string().into()),
                    insert: p.text.clone(),
                    prompt: Some(i),
                }
            })
            .collect();
        if items.is_empty() {
            items.push(CompletionItem {
                label: if self.prompts.is_empty() {
                    "No saved prompts yet"
                } else {
                    "No prompt matches"
                }
                .into(),
                detail: Some(
                    crate::platform::shortcut_label("Write a prompt, then Ctrl+S saves it for !")
                        .into(),
                ),
                insert: draft.to_string(),
                prompt: None,
            });
        }
        Completion {
            token_start: 0,
            token_end: draft.len(),
            items,
            selected: 0,
            prompts: true,
        }
    }

    /// The commands starting with `name` (lowercase): `/stage`, the session's
    /// own, and the skills where its agent reads them.
    fn command_items(&mut self, name: &str, cx: &mut Context<Self>) -> Vec<CompletionItem> {
        let thread = self.frame.thread.clone();
        let skills = self.frame.skills.and_then(|m| self.skill_index(m, cx));
        let reported: &[crate::agent::Choice] = match &thread {
            Some(t) => t.read(cx).commands(),
            None => &[],
        };
        // `/stage` needs a session to know its files; an agent's own `stage` wins.
        let stage = (thread.is_some() && !reported.iter().any(|c| c.name == "stage")).then(|| {
            crate::agent::Choice {
                id: "stage".into(),
                name: "stage".into(),
                description: Some("Stage only the files this session changed".into()),
            }
        });
        // A skill the session reported already shows once.
        let mut seen = std::collections::HashSet::new();
        stage
            .iter()
            .chain(reported)
            .chain(skills.iter().flat_map(|s| s.iter()))
            .filter(|c| seen.insert(c.name.to_lowercase()))
            .filter(|c| name.is_empty() || c.name.to_lowercase().starts_with(name))
            .take(COMPLETION_ROWS)
            .map(|c| CompletionItem {
                label: format!("/{}", c.name).into(),
                detail: c.description.clone().map(SharedString::from),
                insert: format!("/{} ", c.name),
                prompt: None,
            })
            .collect()
    }

    pub(super) fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(comp) = self.completion.take() else {
            return;
        };
        let Some(item) = comp.items.get(comp.selected) else {
            return;
        };
        if let Some(p) = item.prompt.and_then(|i| self.prompts.get_mut(i)) {
            p.uses += 1;
            p.last_used = model::unix_now();
            crate::prompts::store(&self.prompts);
        }
        let draft = &mut self.here_mut().text;
        // Completing in the middle of the text, before a space: no second space.
        let spaced = draft[comp.token_end.min(draft.len())..].starts_with(char::is_whitespace);
        let insert = if spaced {
            item.insert.trim_end_matches(' ')
        } else {
            item.insert.as_str()
        };
        draft.replace(comp.token_start..comp.token_end, insert);
        self.changed(cx);
        cx.notify();
    }
}

/// The files matching `prefix` (lowercase): names that start with it first,
/// then paths that contain it, shorter ones first.
fn file_items(files: &[String], prefix: &str) -> Vec<CompletionItem> {
    let mut scored: Vec<(u8, usize, &String)> = files
        .iter()
        .filter_map(|f| {
            if prefix.is_empty() {
                return Some((2, f.len(), f));
            }
            let lower = f.to_lowercase();
            let name = f.rsplit('/').next().unwrap_or(f);
            if name.to_lowercase().starts_with(prefix) {
                Some((0, f.len(), f))
            } else if lower.contains(prefix) {
                Some((1, f.len(), f))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| a.2.cmp(b.2)));
    scored
        .into_iter()
        .take(COMPLETION_ROWS)
        .map(|(_, _, f)| CompletionItem {
            label: f.clone().into(),
            detail: None,
            insert: format!("@{f} "),
            prompt: None,
        })
        .collect()
}

/// Whether the agent of `session` in `env` sees the skills on this computer,
/// so `/` completion may offer them. A server's environment or project, a
/// sandbox and a headless session each see other files: there only the
/// commands the session reports count. `sandboxed` is the running session's
/// own state: the project's switch applies only to sessions started after it.
pub(in crate::shell) fn host_skills_reach(
    project: &Project,
    env: &Environment,
    session: Option<&str>,
    sandboxed: Option<bool>,
) -> bool {
    env.remote.is_none()
        && !project.is_remote()
        && !sandboxed.unwrap_or(project.sandbox)
        && !session.is_some_and(crate::headless::shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Remote;

    #[test]
    fn host_skills_stay_with_agents_on_this_computer() {
        let reach = |p: &Project, session: Option<&str>, sandboxed: Option<bool>| {
            host_skills_reach(p, &p.environments[0], session, sandboxed)
        };
        let mut project =
            crate::state::new_project(std::path::Path::new("/tmp/arakun-skills-test"));
        assert!(reach(&project, None, None));
        assert!(reach(&project, Some("s-local"), Some(false)));

        crate::headless::set_shown("s-headless", true);
        assert!(!reach(&project, Some("s-headless"), None));
        crate::headless::set_shown("s-headless", false);

        // A running session keeps the sandbox it started with.
        assert!(!reach(&project, Some("s-local"), Some(true)));
        project.sandbox = true;
        assert!(!reach(&project, None, None));
        assert!(reach(&project, Some("s-local"), Some(false)));
        project.sandbox = false;

        project.environments[0].remote = Some(Remote {
            server: "s1".into(),
            machine: "box".into(),
            place: "/srv/app".into(),
        });
        assert!(!reach(&project, None, None));

        let remote = crate::state::new_project(&Remote::project_path("s1/app"));
        assert!(!reach(&remote, None, None));
    }

    #[test]
    fn names_that_start_with_the_prefix_come_first() {
        let files = ["src/lib.rs", "src/shell/composer.rs", "composer.md"].map(String::from);
        let labels: Vec<_> = file_items(&files, "comp")
            .into_iter()
            .map(|i| i.label.to_string())
            .collect();
        assert_eq!(labels, ["composer.md", "src/shell/composer.rs"]);
    }
}
