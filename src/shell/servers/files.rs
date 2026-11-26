//! The files of a server's environments: the Explorer lists its folders, file
//! tabs show its files and `@` completes its paths, all read over there
//! (`server/files.rs`) and sent over the paired connection.
//!
//! Everything is named to the server relative to the environment's folder.
//! Here a file goes by the environment's path joined with that, as a local
//! one does, so the tree and the tabs need nothing of their own.

use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::explorer::{self, FileKind};
use crate::model::Document;

/// The messages a server older than file browsing refuses.
const FILE_KINDS: [&str; 3] = ["list_dir", "read_file", "list_files"];

/// Said where the files would be when the server is too old to send them.
const TOO_OLD: &str = "This server cannot send its files yet. Update arakun-server on it.";

/// Said where the files would be while the server is away.
fn away(machine: &str) -> String {
    format!("{machine} is not connected. Its files show once it is.")
}

/// `rel`, as the server names it, under `env_path`: one part at a time, so it
/// is the same path the Explorer built from the names it listed.
fn under(env_path: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .filter(|p| !p.is_empty())
        .fold(env_path.to_path_buf(), |path, part| path.join(part))
}

/// Whether a file of a server's environment opens as text: the server sends
/// text only, so a picture or a workbook does not, unless it is text too.
fn opens_as_text(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    explorer::file_kind(path) == FileKind::Text
        || matches!(ext.as_deref(), Some("svg" | "csv" | "tsv"))
}

impl Shell {
    /// Asks the server for folders `dirs` of its environment `env_path`. While
    /// it is away the Explorer says so, and it is asked again once it is back.
    pub(in crate::shell) fn server_list_dirs(&mut self, env_path: &Path, dirs: Vec<PathBuf>) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        let sent = match self.connected(&remote.server) {
            Some(client) => {
                for dir in &dirs {
                    if let Some(rel) = explorer::relative(env_path, dir) {
                        client.send(&json!({
                            "type": "list_dir", "place": remote.place, "dir": rel,
                        }));
                    }
                }
                true
            }
            None => false,
        };
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        let ex = &mut env.explorer;
        if sent {
            ex.loading.extend(dirs);
        } else {
            ex.loading.clear();
            ex.failed = Some(away(&remote.machine).into());
        }
    }

    /// Asks the server for the text of file `path` of its environment
    /// `env_path`, for the tab that shows it.
    pub(in crate::shell) fn server_read_file(
        &mut self,
        env_path: &Path,
        path: &Path,
        cx: &mut Context<Self>,
    ) {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.clone()) else {
            return;
        };
        let failed = if !opens_as_text(path) {
            Some("Only text files of a server's environment open here.".to_string())
        } else if let Some(client) = self.connected(&remote.server)
            && let Some(rel) = explorer::relative(env_path, path)
        {
            client.send(&json!({ "type": "read_file", "place": remote.place, "path": rel }));
            None
        } else {
            Some(away(&remote.machine))
        };
        let kind = TabKind::File {
            path: path.to_path_buf(),
        };
        if let Some(tab) = self.doc_tab_mut(env_path, &kind) {
            tab.doc = Some(match failed {
                Some(why) => Document::Failed(why.into()),
                None => Document::Loading,
            });
            cx.notify();
        }
    }

    /// Asks the server for the files `@` completes in its environment
    /// `env_path`. False when it cannot be asked now.
    pub(in crate::shell) fn server_list_files(&self, env_path: &Path) -> bool {
        let Some(remote) = self.find_env(env_path).and_then(|e| e.remote.as_ref()) else {
            return false;
        };
        let Some(client) = self.connected(&remote.server) else {
            return false;
        };
        client.send(&json!({ "type": "list_files", "place": remote.place }));
        true
    }

    /// A message about files, or false when `msg` is not one.
    pub(super) fn server_files_message(
        &mut self,
        server: &str,
        msg: &Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = msg["type"].as_str().unwrap_or("");
        if kind == "error" && older_server_refused(&text(msg, "message"), &FILE_KINDS) {
            self.server_files_failed(server, TOO_OLD, cx);
            return true;
        }
        if !matches!(kind, "dir" | "file" | "files") {
            return false;
        }
        let env_path = Remote::env_path(server, &text(msg, "place"));
        match kind {
            "dir" => {
                let Some(env) = self.find_env_mut(&env_path) else {
                    return true;
                };
                let dir = under(&env_path, &text(msg, "dir"));
                let ex = &mut env.explorer;
                ex.loading.remove(&dir);
                let entries = msg["entries"].as_array().map(|entries| {
                    entries
                        .iter()
                        .filter_map(|e| {
                            let name = e["name"].as_str().filter(|n| !n.contains('/'))?;
                            Some(explorer::Entry {
                                name: name.to_string().into(),
                                path: dir.join(name),
                                is_dir: e["dir"] == true,
                                ignored: e["ignored"] == true,
                            })
                        })
                        .collect::<Vec<_>>()
                });
                match entries {
                    Some(entries) => {
                        if dir == ex.root {
                            ex.failed = None;
                        }
                        ex.children.insert(dir, entries);
                    }
                    // The folder is gone there: the tree loses it.
                    None if dir != ex.root => {
                        ex.expanded.remove(&dir);
                        ex.children.insert(dir, Vec::new());
                    }
                    None => ex.failed = Some(text(msg, "error").into()),
                }
                cx.notify();
            }
            "file" => {
                let path = under(&env_path, &text(msg, "path"));
                let kind = TabKind::File { path: path.clone() };
                if self.doc_tab_mut(&env_path, &kind).is_none() {
                    return true;
                }
                let Some(body) = msg["text"].as_str().map(str::to_string) else {
                    let why = text(msg, "error");
                    self.server_doc_done(&env_path, &kind, Document::Failed(why.into()), cx);
                    return true;
                };
                // Highlighting a large file takes a while: to the side.
                cx.spawn(async move |this, cx| {
                    let name = path.display().to_string();
                    let doc = cx
                        .background_spawn(crate::guard::task(async move {
                            super::super::documents::lines_document(
                                &name,
                                explorer::lines_of(&body),
                                false,
                            )
                        }))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.server_doc_done(&env_path, &kind, doc, cx)
                    });
                })
                .detach();
            }
            _ => {
                let files: Vec<String> = msg["files"]
                    .as_array()
                    .map(|f| {
                        f.iter()
                            .filter_map(|f| f.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if self.composer.file_index_loading.as_ref() == Some(&env_path) {
                    self.composer.file_index_loading = None;
                }
                self.composer.file_index = Some((env_path, Instant::now(), Arc::new(files)));
                // The popup may have been waiting for them.
                self.update_completion(cx);
            }
        }
        true
    }

    /// The document of a server's file tab arrived, or could not.
    fn server_doc_done(
        &mut self,
        env_path: &Path,
        kind: &TabKind,
        doc: Document,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.doc_tab_mut(env_path, kind) {
            tab.doc = Some(doc);
            if tab.markdown.is_some() {
                tab.markdown = super::super::documents::parse_markdown(tab);
            }
            cx.notify();
        }
    }

    /// A request about files the connection dropped before it went: what
    /// waited for it says why, and asks again once the server is back.
    pub(super) fn server_files_lost(&mut self, server: &str, msg: &Value, cx: &mut Context<Self>) {
        let env_path = Remote::env_path(server, &text(msg, "place"));
        let Some(machine) = self
            .find_env(&env_path)
            .and_then(|e| e.remote.as_ref())
            .map(|r| r.machine.to_string())
        else {
            return;
        };
        match msg["type"].as_str() {
            Some("list_dir") => {
                let dir = under(&env_path, &text(msg, "dir"));
                if let Some(env) = self.find_env_mut(&env_path) {
                    env.explorer.loading.remove(&dir);
                    if dir == env.explorer.root && !env.explorer.children.contains_key(&dir) {
                        env.explorer.failed = Some(away(&machine).into());
                    }
                }
            }
            Some("read_file") => {
                let kind = TabKind::File {
                    path: under(&env_path, &text(msg, "path")),
                };
                if let Some(tab) = self.doc_tab_mut(&env_path, &kind) {
                    tab.doc = Some(Document::Failed(away(&machine).into()));
                }
            }
            _ => {
                if self.composer.file_index_loading.as_ref() == Some(&env_path) {
                    self.composer.file_index_loading = None;
                }
            }
        }
        cx.notify();
    }

    /// Nothing about files comes from `server`: what waits for it says `why`.
    fn server_files_failed(&mut self, server: &str, why: &str, cx: &mut Context<Self>) {
        let envs: Vec<PathBuf> = self
            .projects
            .iter_mut()
            .flat_map(|p| p.environments.iter_mut())
            .filter(|e| e.remote.as_ref().is_some_and(|r| r.server == server))
            .map(|e| {
                if !e.explorer.loading.is_empty() {
                    e.explorer.loading.clear();
                    e.explorer.failed = Some(why.to_string().into());
                }
                e.path.clone()
            })
            .collect();
        if self
            .composer
            .file_index_loading
            .as_ref()
            .is_some_and(|e| envs.contains(e))
        {
            self.composer.file_index_loading = None;
        }
        for tab in self.strip.tabs.iter_mut() {
            let waiting = matches!(tab.doc, Some(Document::Loading))
                && matches!(tab.kind, TabKind::File { .. })
                && tab.env.as_ref().is_some_and(|e| envs.contains(e));
            if waiting {
                tab.doc = Some(Document::Failed(why.to_string().into()));
            }
        }
        cx.notify();
    }

    /// Server `server` is connected again: the trees and file tabs of its
    /// environments are read again, as what changed meanwhile was not heard.
    pub(super) fn server_files_back(&mut self, server: &str, cx: &mut Context<Self>) {
        let mut trees = Vec::new();
        let mut envs = Vec::new();
        for env in self.projects.iter().flat_map(|p| p.environments.iter()) {
            if env.remote.as_ref().is_none_or(|r| r.server != server) {
                continue;
            }
            let ex = &env.explorer;
            if ex.failed.is_some() || ex.children.contains_key(&ex.root) {
                trees.push((env.path.clone(), ex.loaded_dirs()));
            }
            envs.push(env.path.clone());
        }
        for (env_path, dirs) in trees {
            self.server_list_dirs(&env_path, dirs);
        }
        let files: Vec<(PathBuf, PathBuf)> = self
            .strip
            .tabs
            .iter()
            .filter_map(|t| match (&t.kind, &t.env) {
                (TabKind::File { path }, Some(env)) if envs.contains(env) => {
                    Some((env.clone(), path.clone()))
                }
                _ => None,
            })
            .collect();
        for (env_path, path) in files {
            self.server_read_file(&env_path, &path, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_server_names_is_the_path_the_tree_built() {
        let env = Remote::env_path("s1", "/srv/notes");
        assert_eq!(under(&env, ""), env);
        assert_eq!(under(&env, "src/main.rs"), env.join("src").join("main.rs"));
        let file = env.join("src").join("main.rs");
        let rel = explorer::relative(&env, &file).unwrap();
        assert_eq!(under(&env, &rel), file);
    }

    #[test]
    fn only_text_opens_from_a_server() {
        assert!(opens_as_text(Path::new("src/main.rs")));
        assert!(opens_as_text(Path::new("Makefile")));
        assert!(opens_as_text(Path::new("logo.svg")));
        assert!(opens_as_text(Path::new("data.csv")));
        assert!(!opens_as_text(Path::new("logo.png")));
        assert!(!opens_as_text(Path::new("book.xlsx")));
    }

    #[test]
    fn an_older_server_refusing_files_is_told_apart() {
        let old = "bad message: unknown variant `list_dir`, expected one of `open`, `close`";
        assert!(older_server_refused(old, &FILE_KINDS));
        let other = "bad message: unknown variant `term_open`, expected one of `list_dir`";
        assert!(!older_server_refused(other, &FILE_KINDS));
    }
}
