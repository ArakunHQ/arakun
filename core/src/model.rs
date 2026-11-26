//! The parts of the app's model the phone and the server speak in.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Working,
    NeedsInput,
    Error,
}

impl Status {
    /// What the session list shows for a session in `self`: work the agent
    /// left running in the background (subagents, the prompt held for them, a
    /// turn of its own) is still work, so the session waits on nobody until it
    /// ends. Only the list: the chat lets the user prompt again meanwhile.
    pub fn listed(self, in_background: bool) -> Status {
        match self {
            Status::Idle if in_background => Status::Working,
            status => status,
        }
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Worktrees arakun creates live in `~/arakun-workspaces/<project>/<branch>`: visible to
/// editors and file managers, and outside every repository.
pub fn workspaces_root() -> std::path::PathBuf {
    crate::platform::home().join("arakun-workspaces")
}

pub fn worktree_path(project: &str, branch: &str) -> std::path::PathBuf {
    let slug: String = branch
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let base = workspaces_root().join(project);
    let mut path = base.join(slug);
    let mut n = 2;
    while path.exists() {
        path = base.join(format!("{slug}-{n}"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_work_keeps_a_session_working() {
        assert_eq!(Status::Idle.listed(true), Status::Working);
        assert_eq!(Status::Idle.listed(false), Status::Idle);
        // A question or a failure still reaches the user while subagents run.
        assert_eq!(Status::NeedsInput.listed(true), Status::NeedsInput);
        assert_eq!(Status::Error.listed(true), Status::Error);
        assert_eq!(Status::Working.listed(false), Status::Working);
    }
}
