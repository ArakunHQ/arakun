//! File tree of an environment, loaded one folder at a time as folders open, so
//! a huge repository costs nothing until it is browsed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::SharedString;

#[derive(Clone)]
pub struct Entry {
    pub name: SharedString,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Matched by .gitignore; shown dimmed like editors do.
    pub ignored: bool,
}

pub struct Explorer {
    pub root: PathBuf,
    pub children: HashMap<PathBuf, Vec<Entry>>,
    pub expanded: HashSet<PathBuf>,
    pub loading: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    /// Why the tree could not be listed, shown in its place: a server's
    /// folder while the server is away, or one too old to list it.
    pub failed: Option<SharedString>,
}

pub struct Row {
    pub depth: usize,
    pub entry: Entry,
    pub expanded: bool,
}

impl Explorer {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            children: HashMap::new(),
            expanded: HashSet::new(),
            loading: HashSet::new(),
            selected: None,
            failed: None,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let Some(children) = self.children.get(dir) else {
            return;
        };
        for entry in children {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            rows.push(Row {
                depth,
                entry: entry.clone(),
                expanded,
            });
            if expanded {
                self.push_rows(&entry.path, depth + 1, rows);
            }
        }
    }

    /// Folders whose listing should be (re)read: the root plus every open folder.
    pub fn loaded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.root.clone()];
        dirs.extend(self.expanded.iter().cloned());
        dirs
    }
}

/// Lists one folder: folders first, then files, case-insensitive. `.git` is hidden.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let visible: HashSet<PathBuf> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .map(|e| e.into_path())
        .collect();
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != ".git")
        .map(|e| {
            let path = e.path();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir())
                || (e.file_type().is_ok_and(|t| t.is_symlink()) && path.is_dir());
            Entry {
                name: e.file_name().to_string_lossy().into_owned().into(),
                ignored: !visible.contains(&path),
                path,
                is_dir,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

pub const MAX_VIEW_BYTES: u64 = 8 << 20;

/// How arakun shows a file it is asked to open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    Text,
    /// Drawn in the file tab.
    Image,
    /// A spreadsheet, shown as a grid in the file tab (src/sheet.rs).
    Sheet,
    /// Left to the system's default app: documents, media, archives.
    External,
}

pub fn file_kind(path: &Path) -> FileKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "bmp" | "tif" | "tiff" | "ico"
        | "pbm" | "pgm" | "ppm" => FileKind::Image,
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" | "csv" | "tsv" => FileKind::Sheet,
        "pdf" | "epub" | "doc" | "docx" | "ppt" | "pptx" | "odt" | "odp" | "rtf" | "pages"
        | "numbers" | "key" | "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "flv"
        | "mp3" | "wav" | "ogg" | "oga" | "opus" | "flac" | "m4a" | "aac" | "mid" | "midi"
        | "avif" | "heic" | "heif" | "psd" | "xcf" | "kra" | "ai" | "sketch" | "fig" | "blend"
        | "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "ttf" | "otf"
        | "woff" | "woff2" | "exe" | "msi" | "dmg" | "iso" | "deb" | "rpm" | "appimage" | "apk"
        | "jar" | "sqlite" | "sqlite3" | "db" => FileKind::External,
        _ => FileKind::Text,
    }
}

/// Lines of a text file for the read-only viewer.
pub fn read_lines(path: &Path) -> Result<Vec<SharedString>, String> {
    read_text(path).map(|text| lines_of(&text))
}

/// The text of a file the viewer can show: not too large, not binary. A
/// server sends this for a file of its own (`read_file`).
pub fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_VIEW_BYTES {
        return Err(format!(
            "File is too large to preview ({:.1} MB)",
            meta.len() as f64 / 1e6
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err("Binary file".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `text` as the viewer's lines: tabs as spaces, no line ends.
pub fn lines_of(text: &str) -> Vec<SharedString> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n')
        .map(|l| SharedString::from(l.trim_end_matches('\r').replace('\t', "    ")))
        .collect()
}

/// `rel` under `root`, when it stays there: no absolute path, no `..`. What a
/// device names in a server's folder is read this way, so it cannot name
/// anything outside it.
pub fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    use std::path::Component;
    let rel = Path::new(rel);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then(|| root.join(rel))
}

/// `path` relative to `root`, with `/` between its parts whatever the
/// platform: how a file in a server's folder is named to the server.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_kinds_by_extension() {
        assert_eq!(file_kind(Path::new("a/logo.PNG")), FileKind::Image);
        assert_eq!(file_kind(Path::new("pelicano.svg")), FileKind::Image);
        assert_eq!(file_kind(Path::new("informe.pdf")), FileKind::External);
        assert_eq!(file_kind(Path::new("demo.mp4")), FileKind::External);
        assert_eq!(file_kind(Path::new("Horas.XLSX")), FileKind::Sheet);
        assert_eq!(file_kind(Path::new("datos.csv")), FileKind::Sheet);
        assert_eq!(file_kind(Path::new("src/main.rs")), FileKind::Text);
        assert_eq!(file_kind(Path::new("Makefile")), FileKind::Text);
    }

    #[test]
    fn a_relative_path_stays_inside_its_folder() {
        let root = Path::new("/srv/notes");
        assert_eq!(inside(root, "src/main.rs"), Some(root.join("src/main.rs")));
        assert_eq!(inside(root, ""), Some(root.to_path_buf()));
        assert_eq!(inside(root, "./a"), Some(root.join("./a")));
        assert_eq!(inside(root, "../etc/passwd"), None);
        assert_eq!(inside(root, "src/../../x"), None);
        assert_eq!(inside(root, "/etc/passwd"), None);
    }

    #[test]
    fn a_path_is_named_relative_to_its_folder_with_slashes() {
        let root = Path::new("arakun-server://s1/srv/notes");
        let file = root.join("src").join("main.rs");
        assert_eq!(relative(root, &file).as_deref(), Some("src/main.rs"));
        assert_eq!(relative(root, root).as_deref(), Some(""));
        assert_eq!(relative(root, Path::new("/elsewhere")), None);
    }

    #[test]
    fn text_becomes_lines_without_their_ends() {
        let lines = lines_of("a\tb\r\nc\n");
        assert_eq!(lines, vec![SharedString::from("a    b"), "c".into()]);
    }
}
