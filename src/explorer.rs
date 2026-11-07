//! Lazily expanded filesystem tree with persistent path-based marks.
use crate::file_transfer::STASH_PREFIX;
use anyhow::Result;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// Image extensions that receive the landscape icon, including text-encoded SVGs.
const IMAGE_EXTENSIONS: &[&str] = &[
    "apng", "avif", "bmp", "dds", "exr", "gif", "heic", "heif", "ico", "jpeg", "jpg", "jxl", "png",
    "psd", "qoi", "svg", "svgz", "tif", "tiff", "webp",
];

/// Common plain-text, source and configuration extensions recognized by the file tree.
const TEXT_EXTENSIONS: &[&str] = &[
    "adoc",
    "asm",
    "bash",
    "bat",
    "c",
    "cc",
    "cfg",
    "cjs",
    "cmd",
    "conf",
    "cpp",
    "cs",
    "css",
    "csv",
    "cts",
    "dart",
    "diff",
    "env",
    "fish",
    "go",
    "gql",
    "graphql",
    "h",
    "hpp",
    "htm",
    "html",
    "ini",
    "java",
    "js",
    "json",
    "json5",
    "jsonc",
    "jsx",
    "kt",
    "kts",
    "less",
    "lock",
    "log",
    "lua",
    "markdown",
    "md",
    "mjs",
    "mts",
    "nix",
    "patch",
    "php",
    "pl",
    "properties",
    "ps1",
    "psd1",
    "psm1",
    "py",
    "pyi",
    "r",
    "rb",
    "rs",
    "rst",
    "sass",
    "scss",
    "sh",
    "sql",
    "svelte",
    "swift",
    "tex",
    "text",
    "toml",
    "ts",
    "tsv",
    "tsx",
    "txt",
    "vue",
    "xml",
    "yaml",
    "yml",
    "zig",
    "zsh",
];

/// Conventional text filenames without a recognized extension.
const TEXT_NAMES: &[&str] = &[
    ".bashrc",
    ".dockerignore",
    ".editorconfig",
    ".env",
    ".gitattributes",
    ".gitignore",
    ".gitmodules",
    ".npmrc",
    ".profile",
    ".vimrc",
    ".zshrc",
    "authors",
    "changelog",
    "containerfile",
    "copying",
    "dockerfile",
    "gemfile",
    "justfile",
    "license",
    "makefile",
    "notice",
    "readme",
];

/// One visible file-manager entry.
pub struct Entry {
    /// Full filesystem path.
    pub path: PathBuf,
    /// Relative nesting depth below the project root.
    pub depth: usize,
    /// Whether this entry is an actual directory rather than a followed symlink.
    pub directory: bool,
}

/// Bundled Lucide-style glyphs used by the file tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryIcon {
    /// A collapsed directory.
    Folder,
    /// An expanded directory.
    FolderOpen,
    /// A recognized text file.
    Text,
    /// A recognized image file.
    Image,
    /// The question-mark document used as the fallback for other files.
    Unknown,
}

impl EntryIcon {
    /// Return the stable GPUI sprite-cache key and the embedded SVG bytes.
    pub fn svg(self) -> (&'static str, &'static [u8]) {
        match self {
            Self::Folder => (
                "explorer/folder.svg",
                include_bytes!("../assets/icons/folder.svg"),
            ),
            Self::FolderOpen => (
                "explorer/folder-open.svg",
                include_bytes!("../assets/icons/folder-open.svg"),
            ),
            Self::Text => (
                "explorer/file-text.svg",
                include_bytes!("../assets/icons/file-text.svg"),
            ),
            Self::Image => (
                "explorer/image.svg",
                include_bytes!("../assets/icons/image.svg"),
            ),
            Self::Unknown => (
                "explorer/unknown-file.svg",
                include_bytes!("../assets/icons/unknown-file.svg"),
            ),
        }
    }
}

impl Entry {
    /// Choose a directory icon from `expanded` or classify the file by name.
    pub fn icon(&self, expanded: bool) -> EntryIcon {
        if self.directory {
            return if expanded {
                EntryIcon::FolderOpen
            } else {
                EntryIcon::Folder
            };
        }
        let extension = self
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some(ext) if IMAGE_EXTENSIONS.contains(&ext) => EntryIcon::Image,
            Some(ext) if TEXT_EXTENSIONS.contains(&ext) => EntryIcon::Text,
            _ => {
                let name = self
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if TEXT_NAMES.contains(&name.as_str()) || name.starts_with(".env.") {
                    EntryIcon::Text
                } else {
                    EntryIcon::Unknown
                }
            }
        }
    }
}

/// Project tree state, retained while focus moves into the editor.
pub struct Explorer {
    /// Current project directory.
    pub root: PathBuf,
    /// Flattened visible tree, sorted with directories first.
    pub entries: Vec<Entry>,
    /// Selected visible entry.
    pub selected: usize,
    /// Files and directories marked for a batch operation, including collapsed descendants.
    pub marked: HashSet<PathBuf>,
    /// Directories whose immediate children have been requested.
    pub expanded: HashSet<PathBuf>,
}

impl Explorer {
    /// Scan the first level of `root`, without recursively indexing the project.
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root,
            entries: Vec::new(),
            selected: 0,
            marked: HashSet::new(),
            expanded: HashSet::new(),
        };
        let _ = tree.refresh();
        tree
    }

    /// Refresh visible entries and retain the selected path when it still exists.
    pub fn refresh(&mut self) -> Result<()> {
        let selected = self.entries.get(self.selected).map(|e| e.path.clone());
        let mut entries = Vec::new();
        self.walk(&self.root, 0, &mut entries)?;
        self.entries = entries;
        self.selected = selected
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or(0);
        self.marked.retain(|path| !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound));
        Ok(())
    }

    /// Toggle the current entry's mark, returning the number of marked paths.
    pub fn toggle_mark(&mut self) -> usize {
        if let Some(entry) = self.entries.get(self.selected)
            && !self.marked.remove(&entry.path)
        {
            self.marked.insert(entry.path.clone());
        }
        self.marked.len()
    }

    /// Use the current directory, or the containing directory of a file, as the paste destination.
    pub fn directory(&self) -> &Path {
        match self.entries.get(self.selected) {
            Some(entry) if entry.directory => &entry.path,
            Some(entry) => entry.path.parent().unwrap_or(&self.root),
            None => &self.root,
        }
    }

    /// Append children of `directory` at `depth`, descending only into explicitly expanded nodes.
    fn walk(&self, directory: &Path, depth: usize, output: &mut Vec<Entry>) -> Result<()> {
        if depth > 32 || output.len() >= 10_000 {
            return Ok(());
        }
        let mut children: Vec<_> = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter(|entry| {
                directory != self.root
                    || !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(STASH_PREFIX)
            })
            .filter_map(|e| {
                let kind = e.file_type().ok()?;
                Some((e.path(), kind.is_dir()))
            })
            .collect();
        children.sort_by_key(|(p, dir)| {
            (
                !dir,
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase(),
            )
        });
        for (path, directory) in children {
            if output.len() >= 10_000 {
                break;
            }
            let open = directory && self.expanded.contains(&path);
            output.push(Entry {
                path: path.clone(),
                depth,
                directory,
            });
            if open {
                let _ = self.walk(&path, depth + 1, output);
            }
        }
        Ok(())
    }

    /// Activate the selected entry; return a file to open or toggle a directory's expansion.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?;
        if !entry.directory {
            return Some(entry.path.clone());
        }
        let path = entry.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        let _ = self.refresh();
        None
    }

    /// When `expand` is true, expand the selected directory. Otherwise fold the nearest branch.
    pub fn branch(&mut self, expand: bool) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        let path = entry.path.clone();
        if expand && entry.directory {
            self.expanded.insert(path);
        } else if !expand
            && !self.expanded.remove(&path)
            && let Some(parent) = path.parent()
            && let Some(index) = self.entries.iter().position(|e| e.path == parent)
        {
            self.selected = index;
            self.expanded.remove(parent);
        }
        let _ = self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Directories, recognized files and unknown files each use their matching icon.
    #[test]
    fn entry_icons_respect_expansion_and_file_kind() {
        let icon = |name: &str, directory, expanded| {
            Entry {
                path: name.into(),
                depth: 0,
                directory,
            }
            .icon(expanded)
        };
        assert_eq!(icon("art.svg", true, false), EntryIcon::Folder);
        assert_eq!(icon("art.svg", true, true), EntryIcon::FolderOpen);
        for name in ["art.SVG", "壁纸.JPEG", "preview.png"] {
            assert_eq!(icon(name, false, false), EntryIcon::Image);
        }
        for name in ["app.RS", "README", ".gitignore", ".env.local", "Cargo.lock"] {
            assert_eq!(icon(name, false, false), EntryIcon::Text);
        }
        for name in ["archive.bin", "Nanobug.exe", "unknown"] {
            assert_eq!(icon(name, false, false), EntryIcon::Unknown);
        }
    }
}
