//! What the palette reads besides the shell's state: the project's files and
//! the symbols in them (on a background thread), and what Arakun remembers
//! (the memory stores, read in process).

use std::path::Path;

use crate::memory::{self, Memory, Scope, Store};

/// Files the walk stops at: plenty for a repository, cheap to rank.
const MAX_FILES: usize = 30_000;
/// Files read for symbols, and how big one may be.
const MAX_SYMBOL_FILES: usize = 6_000;
const MAX_SYMBOL_BYTES: u64 = 512 * 1024;

/// The project's files as paths relative to `root`, as git would list them
/// (ignored files left out). Blocking: run it off the UI thread.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .build();
    for entry in walk.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
    out
}

/// A definition in a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// `fn card_content`, `struct CardLayout`.
    pub kind: &'static str,
    pub name: String,
    /// The `impl` it sits in, for methods: `CardLayout`.
    pub owner: Option<String>,
    pub file: String,
    pub line: u32,
}

impl Symbol {
    pub fn label(&self) -> String {
        match &self.owner {
            Some(o) => format!("{o}::{}", self.name),
            None => self.name.clone(),
        }
    }
}

const SOURCE: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "go", "swift", "kt", "java", "rb", "c", "h", "cpp",
];

/// The definitions in one file's text, by its extension.
pub fn symbols_in(file: &str, text: &str) -> Vec<Symbol> {
    let ext = file.rsplit('.').next().unwrap_or("");
    let mut out = Vec::new();
    let mut owner: Option<(String, usize)> = None;
    for (i, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim_start();
        // An `impl` block owns the indented `fn`s after it, until it closes.
        if let Some((_, at)) = &owner
            && indent <= *at
            && !line.is_empty()
            && !line.starts_with("impl")
            && line.starts_with('}')
        {
            owner = None;
        }
        let strip = |l: &'static str| -> Option<&str> { line.strip_prefix(l) };
        let found: Option<(&'static str, &str)> = match ext {
            "rs" => {
                let l = line
                    .trim_start_matches("pub(crate) ")
                    .trim_start_matches("pub(super) ")
                    .trim_start_matches("pub ")
                    .trim_start_matches("async ")
                    .trim_start_matches("const ")
                    .trim_start_matches("unsafe ");
                if let Some(rest) = line.strip_prefix("impl") {
                    // `impl<T> Wrapper<T>`: the owner comes after the generics.
                    let rest = after_generics(rest.trim_start());
                    let rest = rest.split(" for ").last().unwrap_or(rest);
                    let name: String = rest
                        .trim_start_matches(|c: char| !c.is_alphanumeric())
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        owner = Some((name, indent));
                    }
                    None
                } else {
                    [
                        "fn ",
                        "struct ",
                        "enum ",
                        "trait ",
                        "type ",
                        "mod ",
                        "macro_rules! ",
                    ]
                    .iter()
                    .find_map(|k| {
                        l.strip_prefix(k)
                            .map(|r| (k.trim_end_matches(['!', ' ']), r))
                    })
                }
            }
            "py" => strip("def ")
                .map(|r| ("def", r))
                .or_else(|| strip("class ").map(|r| ("class", r))),
            "go" => strip("func ")
                .map(|r| ("func", r))
                .or_else(|| strip("type ").map(|r| ("type", r))),
            _ => {
                let l = line
                    .trim_start_matches("export ")
                    .trim_start_matches("default ")
                    .trim_start_matches("async ");
                ["function ", "class ", "interface ", "type ", "enum "]
                    .iter()
                    .find_map(|k| l.strip_prefix(k).map(|r| (k.trim_end(), r)))
            }
        };
        let Some((kind, rest)) = found else { continue };
        let rest = rest.trim_start_matches('(');
        // Go methods: `func (r *Repo) Name(`.
        let rest = if ext == "go" && line.starts_with("func (") {
            rest.split_once(')')
                .map(|(_, r)| r.trim_start())
                .unwrap_or(rest)
        } else {
            rest
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if name.is_empty() {
            continue;
        }
        let owner = match (&owner, kind) {
            (Some((o, at)), "fn") if indent > *at => Some(o.clone()),
            _ => None,
        };
        out.push(Symbol {
            kind,
            name,
            owner,
            file: file.to_string(),
            line: i as u32 + 1,
        });
    }
    out
}

/// Every definition in the project's source files. Blocking.
/// `s` past a leading `<...>` group, nested ones included. The `>` of an
/// arrow (`Fn() -> u8`) does not close it.
fn after_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0usize;
    let mut prev = ' ';
    for (at, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' if prev != '-' => {
                depth -= 1;
                if depth == 0 {
                    return s[at + 1..].trim_start();
                }
            }
            _ => {}
        }
        prev = c;
    }
    ""
}

pub fn index_symbols(root: &Path, files: &[String]) -> Vec<Symbol> {
    let mut out = Vec::new();
    for rel in files
        .iter()
        .filter(|f| SOURCE.contains(&f.rsplit('.').next().unwrap_or("")))
        .take(MAX_SYMBOL_FILES)
    {
        let path = root.join(rel);
        if std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(u64::MAX)
            > MAX_SYMBOL_BYTES
        {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.extend(symbols_in(rel, &text));
        }
    }
    out
}

/// Some lines of a file around `line` (1-based) for the preview, numbered.
pub fn snippet(
    path: &Path,
    line: Option<u32>,
    count: usize,
) -> Option<(Vec<(u32, String)>, usize)> {
    if std::fs::metadata(path).ok()?.len() > 4 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(4096).any(|b| *b == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = line
        .map(|l| (l as usize).saturating_sub(1))
        .unwrap_or(0)
        .min(total.saturating_sub(1));
    let shown = lines
        .iter()
        .enumerate()
        .skip(start)
        .take(count)
        .map(|(i, l)| (i as u32 + 1, l.replace('\t', "    ")))
        .collect();
    Some((shown, total))
}

// ---- memory ----------------------------------------------------------------------------

/// The project's and the global memory stores for a directory.
pub fn memory_stores(dir: &Path) -> Vec<(Scope, Store)> {
    let root = memory::root();
    let project = memory::project_of(dir);
    vec![
        (
            Scope::Project,
            memory::load(&memory::store_file(&root, Scope::Project, &project)),
        ),
        (
            Scope::Global,
            memory::load(&memory::store_file(&root, Scope::Global, Path::new(""))),
        ),
    ]
}

/// A memory that matched, with where it lives.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryHit {
    pub scope: Scope,
    pub score: f32,
    pub memory: Memory,
}

impl MemoryHit {
    /// `#12` or `#g3`, the id `arakun mem` takes.
    pub fn id(&self) -> String {
        match self.scope {
            Scope::Project => format!("#{}", self.memory.id),
            Scope::Global => format!("#g{}", self.memory.id),
        }
    }
}

/// The best memories for `query` across the stores, project ones a little
/// ahead on ties. An empty query gives the newest.
#[cfg(test)]
fn memory_hits(stores: &[(Scope, Store)], query: &str, limit: usize) -> Vec<MemoryHit> {
    let docs: Vec<_> = stores
        .iter()
        .map(|(_, s)| memory::index(&s.items))
        .collect();
    memory_hits_in(stores, &docs, query, limit)
}

/// `memory_hits` with each store's words read once by `memory::index`.
pub fn memory_hits_in(
    stores: &[(Scope, Store)],
    docs: &[Vec<Vec<String>>],
    query: &str,
    limit: usize,
) -> Vec<MemoryHit> {
    let mut out: Vec<MemoryHit> = Vec::new();
    for ((scope, store), docs) in stores.iter().zip(docs) {
        if query.trim().is_empty() {
            out.extend(store.items.iter().map(|m| MemoryHit {
                scope: *scope,
                score: 0.,
                memory: m.clone(),
            }));
        } else {
            let lift = if *scope == Scope::Project { 1.05 } else { 1. };
            out.extend(
                memory::search_in(&store.items, docs, query)
                    .into_iter()
                    .map(|(s, m)| MemoryHit {
                        scope: *scope,
                        score: s * lift,
                        memory: m.clone(),
                    }),
            );
        }
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.memory.updated.cmp(&a.memory.updated))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: u64, title: &str, body: &str, updated: u64) -> Memory {
        Memory {
            id,
            title: title.into(),
            kind: "gotcha".into(),
            body: body.into(),
            topic: None,
            files: vec![],
            created: 0,
            updated,
            hits: 0,
            revisions: 0,
        }
    }

    #[test]
    fn rust_symbols_with_their_impl() {
        let text = "pub struct CardLayout {\n}\n\nimpl CardLayout {\n    pub fn measure(&self) {}\n}\n\nfn card_content() {}\nimpl<T> Show for Wrapper<T> {\n    fn show(&self) {}\n}\n";
        let s = symbols_in("src/island.rs", text);
        let labels: Vec<(String, u32)> = s.iter().map(|s| (s.label(), s.line)).collect();
        assert_eq!(
            labels,
            vec![
                ("CardLayout".to_string(), 1),
                ("CardLayout::measure".to_string(), 5),
                ("card_content".to_string(), 8),
                ("Wrapper::show".to_string(), 10),
            ]
        );
        assert_eq!(s[0].kind, "struct");
        assert_eq!(s[1].kind, "fn");
    }

    #[test]
    fn generic_inherent_impls_name_their_type() {
        let text = "impl<T> Wrapper<T> {\n    fn new() {}\n}\nimpl<'a> Foo<'a> {\n    fn get(&self) {}\n}\nimpl<F: Fn() -> u8, T: Into<Vec<u8>>> Café<F, T> {\n    fn run(&self) {}\n}\n";
        let labels: Vec<String> = symbols_in("src/a.rs", text)
            .iter()
            .map(|s| s.label())
            .collect();
        assert_eq!(labels, vec!["Wrapper::new", "Foo::get", "Café::run"]);
        assert_eq!(after_generics("<T"), "");
    }

    #[test]
    fn other_languages() {
        let ts = symbols_in(
            "a.ts",
            "export function relay(url: string) {}\nexport default class Worker {}\n",
        );
        assert_eq!(
            ts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["relay", "Worker"]
        );
        let py = symbols_in("a.py", "def main():\n    pass\nclass Thing:\n");
        assert_eq!(
            py.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["main", "Thing"]
        );
        let go = symbols_in("a.go", "func (r *Repo) Name() string {\nfunc main() {\n");
        assert_eq!(
            go.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Name", "main"]
        );
    }

    #[test]
    fn memory_search_spans_both_scopes() {
        let project = Store {
            items: vec![
                mem(
                    1,
                    "Island: matar island-host por pid tras instalar",
                    "pkill -f no",
                    10,
                ),
                mem(
                    2,
                    "cargo fmt --all formatea lienzo",
                    "usa cargo fmt sin --all",
                    20,
                ),
            ],
            ..Default::default()
        };
        let global = Store {
            items: vec![mem(
                3,
                "Instalar con tools/install.sh",
                "sh tools/install.sh",
                30,
            )],
            ..Default::default()
        };
        let stores = vec![(Scope::Project, project), (Scope::Global, global)];
        let hits = memory_hits(&stores, "instalar", 10);
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .any(|h| h.scope == Scope::Global && h.id() == "#g3")
        );
        assert!(
            hits.iter()
                .any(|h| h.scope == Scope::Project && h.id() == "#1")
        );
        assert!(memory_hits(&stores, "zebra", 10).is_empty());
        // Nothing typed: the newest first.
        let newest = memory_hits(&stores, "", 2);
        assert_eq!(
            newest.iter().map(|h| h.memory.id).collect::<Vec<_>>(),
            [3, 2]
        );
    }

    #[test]
    fn memory_stores_read_from_the_memory_dir() {
        // `ARAKUN_MEMORY_DIR` is read per call; a missing dir gives empty stores.
        let stores = memory_stores(Path::new("/nonexistent/arakun-test-dir"));
        assert_eq!(stores.len(), 2);
        assert_eq!(stores[0].0, Scope::Project);
        assert_eq!(stores[1].0, Scope::Global);
    }

    #[test]
    fn snippets_start_at_the_line() {
        let dir = std::env::temp_dir().join(format!("arakun-snip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.rs");
        std::fs::write(&f, "one\ntwo\nthree\nfour\n").unwrap();
        let (lines, total) = snippet(&f, Some(2), 2).unwrap();
        assert_eq!(total, 4);
        assert_eq!(
            lines,
            vec![(2, "two".to_string()), (3, "three".to_string())]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_are_listed_relative() {
        let dir = std::env::temp_dir().join(format!("arakun-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();
        let files = list_files(&dir);
        assert_eq!(
            files,
            vec!["README.md".to_string(), "src/main.rs".to_string()]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
