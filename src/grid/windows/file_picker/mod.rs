//! Directory navigation, text prompts, and selection state for the file picker.

mod preview;
mod view;

pub(crate) use preview::{FilePreviewProvider, PluginPreview, PreviewHighlighter, PreviewShortcut};
pub(in crate::grid) use preview::{Preview, SyntaxColors};
pub(in crate::grid) use view::PickerLayout;

use super::{Shortcut, ShortcutKeys::Fixed};
use crate::{grid::mnemonic::Mnemonic, ui::ime::Composition};
use anyhow::{Context, Result, ensure};
use nanobug::i18n;
use std::{
    borrow::Cow,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;

/// Bindings for browsing files to open in the workspace.
pub(in crate::grid) const SHORTCUTS: &[Shortcut] = &shortcuts("picker.open_help");

/// Bindings for choosing a file for the caller.
pub(in crate::grid) const CHOOSE_SHORTCUTS: &[Shortcut] = &shortcuts("picker.choose_help");

/// Share picker help rows with the localized `enter` description for the current action.
const fn shortcuts(enter: &'static str) -> [Shortcut; 15] {
    [
        (Fixed("h / l  ← / →"), "picker.browse"),
        (Fixed("j / k  ↓ / ↑"), "picker.move"),
        (Fixed("gg / G"), "picker.ends"),
        (Fixed("g  gh  gd  g Space"), "picker.goto_help"),
        (Fixed("f"), "picker.filter_help"),
        (Fixed("/"), "picker.search_help"),
        (Fixed("n / N"), "picker.search_navigation_help"),
        (Fixed("Enter"), enter),
        (Fixed("O"), "picker.external_help"),
        (Fixed("J / K"), "picker.preview_scroll"),
        (Fixed("s"), "picker.mark_help"),
        (Fixed("y / c  x  v / p"), "picker.transfer_help"),
        (Fixed("a  r  d / D"), "picker.mutation_help"),
        (Fixed("<leader>q / Esc"), "picker.close_help"),
        (Fixed("Esc"), "picker.cancel_help"),
    ]
}

/// Help for the read-only text pane in the workspace browser.
pub(in crate::grid) const TEXT_PREVIEW_SHORTCUTS: &[Shortcut] =
    &text_preview_shortcuts("editor_shortcuts.open");

/// Help for the read-only text pane while choosing a file.
pub(in crate::grid) const CHOOSE_PREVIEW_SHORTCUTS: &[Shortcut] =
    &text_preview_shortcuts("picker.preview_locate_help");

/// Share Vim bindings with the localized `open` description for the preview's :e command.
const fn text_preview_shortcuts(open: &'static str) -> [Shortcut; 12] {
    [
        (Fixed("Esc / :q"), "picker.preview_back_help"),
        (Fixed("h j k l / arrows"), "editor_shortcuts.move"),
        (Fixed("w b e  0 ^ $"), "editor_shortcuts.word_motion"),
        (Fixed("gg / G / 3G"), "editor_shortcuts.line_jump"),
        (Fixed("PageUp / PageDown"), "editor_shortcuts.page"),
        (Fixed("Ctrl+d/u  zt/zz/zb"), "editor_shortcuts.page"),
        (Fixed("v / V / Ctrl+v"), "picker.preview_select_help"),
        (Fixed("y / yy / yiw"), "picker.preview_yank_help"),
        (Fixed("/ ?  n N  * #"), "picker.preview_search_help"),
        (Fixed("m  '  `  Ctrl+o/i"), "picker.preview_marks_help"),
        (Fixed("q / @ / Q"), "picker.preview_macro_help"),
        (Fixed(":e"), open),
    ]
}

/// One listed filesystem entry with the kind used for browsing and display.
pub(in crate::grid) struct PickerEntry {
    /// Native absolute path, kept separate from the display label.
    pub path: PathBuf,
    /// Whether this row can be entered with l.
    pub directory: bool,
}

impl PickerEntry {
    /// Return the basename, or the whole path for a volume root.
    fn name(&self) -> Cow<'_, str> {
        self.path
            .file_name()
            .map_or_else(|| display_path(&self.path), |name| name.to_string_lossy())
    }
}

/// Location requested by a directory motion or a typed path.
pub(in crate::grid) enum Browse {
    /// List a directory at path and optionally select a specific child.
    Path(PathBuf, Option<PathBuf>),
    /// Open a remembered directory, using another location if reading it fails.
    RememberedDirectory(PathBuf, Box<Browse>),
    /// List Windows volume roots, optionally selecting the one just left.
    #[cfg(windows)]
    Drives(Option<PathBuf>),
}

impl Browse {
    /// Reveal `directory` in its parent or, for a Windows drive root, in the drive list.
    /// Browse the directory itself when it has no parent in the picker.
    pub fn reveal_directory(directory: &Path) -> Self {
        if let Some(parent) = directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            return Self::Path(parent.to_path_buf(), Some(directory.to_path_buf()));
        }
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};

            if directory.has_root()
                && let Some(Component::Prefix(prefix)) = directory.components().next()
                && matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            {
                return Self::Drives(Some(directory.to_path_buf()));
            }
        }
        Self::Path(directory.to_path_buf(), None)
    }
}

/// The two directory lists obtained in one background scan.
pub(in crate::grid) struct Listing {
    /// Canonical current directory, or None for the Windows volume list.
    pub directory: Option<PathBuf>,
    /// Siblings in the parent directory.
    pub parent: Vec<PickerEntry>,
    /// Children of the current directory.
    pub entries: Vec<PickerEntry>,
    /// Preferred selection after navigating or refreshing.
    pub selected: Option<PathBuf>,
}

impl Listing {
    /// Resolve `request` and scan its directories, matching a preferred child under the resolved parent.
    /// `extensions` restrict files in both lists to the given suffixes without a dot. Empty permits all.
    pub fn read(request: Browse, extensions: &[&str]) -> Result<Self> {
        let (path, selected, fallback) = match request {
            Browse::Path(path, selected) => (path, selected, None),
            Browse::RememberedDirectory(path, fallback) => (path, None, Some(fallback)),
            #[cfg(windows)]
            Browse::Drives(selected) => {
                return Ok(Self {
                    directory: None,
                    parent: Vec::new(),
                    entries: drives(),
                    selected,
                });
            }
        };
        let resolved = fs::canonicalize(&path);
        if let Some(fallback) = fallback {
            return resolved
                .ok()
                .and_then(|directory| Self::read_directory(directory, None, extensions).ok())
                .map_or_else(|| Self::read(*fallback, extensions), Ok);
        }
        let resolved = resolved.with_context(|| path.display().to_string())?;
        let (directory, selected) = if resolved.is_dir() {
            let selected = selected.map(|child| {
                if child.parent() == Some(path.as_path())
                    && let Some(name) = child.file_name()
                {
                    resolved.join(name)
                } else {
                    child
                }
            });
            (resolved, selected)
        } else {
            let parent = resolved
                .parent()
                .context(i18n::text("picker.no_directory"))?;
            (parent.to_path_buf(), Some(resolved))
        };
        Self::read_directory(directory, selected, extensions)
    }

    /// Scan the resolved `directory` and its parent, with `selected` and the file `extensions`.
    fn read_directory(
        directory: PathBuf,
        selected: Option<PathBuf>,
        extensions: &[&str],
    ) -> Result<Self> {
        let entries = list(&directory, extensions)?;
        let parent = directory.parent().map_or_else(
            || {
                #[cfg(windows)]
                return drives();
                #[cfg(not(windows))]
                return Vec::new();
            },
            |parent| list(parent, extensions).unwrap_or_default(),
        );
        Ok(Self {
            directory: Some(directory),
            parent,
            entries,
            selected,
        })
    }
}

/// Enumerate `path` in Explorer order, keeping directories and files matching `extensions`.
fn list(path: &Path, extensions: &[&str]) -> Result<Vec<PickerEntry>> {
    let entries = nanobug::filesystem::read_directory(
        path,
        nanobug::filesystem::DirectoryOptions {
            follow_directory_links: true,
            hide_transfer_staging: true,
        },
        |file| {
            extensions.is_empty()
                || file.extension().is_some_and(|extension| {
                    extensions
                        .iter()
                        .any(|kind| extension.eq_ignore_ascii_case(kind))
                }) && file.is_file()
        },
    )?;
    Ok(entries
        .into_iter()
        .map(|entry| PickerEntry {
            path: entry.path,
            directory: entry.directory,
        })
        .collect())
}

/// Enumerate mounted Windows drive letters without probing each device.
#[cfg(windows)]
fn drives() -> Vec<PickerEntry> {
    // GetLogicalDrives reads a system bitmask and does not dereference pointers.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (b'A'..=b'Z')
        .filter(|letter| mask & (1 << (letter - b'A')) != 0)
        .map(|letter| PickerEntry {
            path: PathBuf::from(format!("{}:\\", char::from(letter))),
            directory: true,
        })
        .collect()
}

/// Show Windows canonical paths in conventional drive or UNC form.
fn display_path(path: &Path) -> Cow<'_, str> {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Cow::Owned(format!(r"\\{unc}"));
        }
        if let Some(drive) = text.strip_prefix(r"\\?\") {
            return Cow::Owned(drive.to_owned());
        }
    }
    text
}

/// Match a listing path to a preferred row, including Windows drive-root aliases.
fn same_row(path: &Path, preferred: &Path) -> bool {
    path == preferred
        || cfg!(windows)
            && path.file_name().is_none()
            && preferred.file_name().is_none()
            && display_path(path).eq_ignore_ascii_case(&display_path(preferred))
}

/// Match a literal name fragment, ignoring case when `query` contains no uppercase letters.
fn name_matches(query: &str) -> impl Fn(&str) -> bool + '_ {
    let exact_case = query.chars().any(char::is_uppercase);
    let folded = query.to_lowercase();
    move |name| {
        if exact_case || query.is_empty() {
            name.contains(query)
        } else {
            name.to_lowercase().contains(&folded)
        }
    }
}

/// Operation whose input is currently being edited.
pub(in crate::grid) enum PromptKind {
    /// Filter the current directory.
    Filter,
    /// Highlight and locate matching entries in the current list.
    Search,
    /// Navigate to an absolute or directory-relative path.
    Jump,
    /// Create a child of the current directory.
    Create,
    /// Rename this selected path.
    Rename(PathBuf),
}

impl PromptKind {
    /// Whether this prompt uses the floating input box below the headings.
    fn is_floating(&self) -> bool {
        matches!(self, Self::Filter | Self::Search)
    }
}

/// Single-line edit buffer with a grapheme-aligned selection.
pub(in crate::grid) struct Prompt {
    /// Action taken when the input is accepted.
    pub kind: PromptKind,
    /// Committed input.
    pub value: String,
    /// Insertion endpoint in bytes.
    caret: usize,
    /// Other endpoint of the selected range in bytes.
    anchor: usize,
}

impl Prompt {
    /// Initialize kind with text and select its prefix through end.
    pub fn new(kind: PromptKind, value: String, end: usize) -> Self {
        Self {
            kind,
            value,
            caret: end,
            anchor: 0,
        }
    }

    /// Replace the selection with single-line committed text.
    pub fn insert(&mut self, text: &str) {
        let start = self.anchor.min(self.caret);
        let end = self.anchor.max(self.caret);
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.value.replace_range(start..end, &text);
        self.caret = start + text.len();
        self.anchor = self.caret;
    }

    /// Apply an editing key to the text, returning whether it was handled.
    pub fn edit(&mut self, key: &str) -> bool {
        let left = self.value[..self.caret]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        let right = self.caret
            + self.value[self.caret..]
                .graphemes(true)
                .next()
                .map_or(0, str::len);
        if matches!(key, "backspace" | "delete") {
            if self.anchor == self.caret {
                self.anchor = if key == "backspace" { left } else { right };
            }
            self.insert("");
        } else if key == "select-all" {
            self.anchor = 0;
            self.caret = self.value.len();
        } else {
            self.caret = match key {
                "left" => {
                    if self.anchor != self.caret {
                        self.anchor.min(self.caret)
                    } else {
                        left
                    }
                }
                "right" => {
                    if self.anchor != self.caret {
                        self.anchor.max(self.caret)
                    } else {
                        right
                    }
                }
                "home" => 0,
                "end" => self.value.len(),
                _ => return false,
            };
            self.anchor = self.caret;
        }
        true
    }
}

/// A deletion awaiting explicit confirmation.
pub(in crate::grid) struct Delete {
    /// Deduplicated paths selected for deletion.
    pub paths: Vec<PathBuf>,
    /// Whether to delete permanently rather than use the recycle bin.
    pub permanent: bool,
}

/// Draw and match the same localized Yes and No access keys.
pub(in crate::grid) fn delete_buttons() -> [(bool, Mnemonic); 2] {
    [
        (true, Mnemonic::parse(i18n::text("picker.yes"))),
        (false, Mnemonic::parse(i18n::text("picker.no"))),
    ]
}

/// Per-opening browser state, independent of Explorer's expanded tree.
#[derive(Default)]
pub(in crate::grid) struct FilePicker {
    /// The last completed directory scan.
    pub listing: Option<Listing>,
    /// Committed filter shown in the current column heading.
    pub filter: String,
    /// Committed name search for this directory.
    pub search: String,
    /// Sorted indices in the visible list that match the active search input.
    search_rows: Vec<usize>,
    /// Indices of matching entries in the scan's order.
    pub visible: Vec<usize>,
    /// Index within the filtered list.
    pub selected: usize,
    /// First visible middle-column row.
    pub first: usize,
    /// First visible row in the right column.
    pub preview_first: usize,
    /// Marked paths, retained across directory changes.
    pub marked: HashSet<PathBuf>,
    /// Active text prompt, when present.
    pub prompt: Option<Prompt>,
    /// Native composition local to the prompt.
    pub composition: Composition,
    /// Whether g is waiting for its second key.
    pub goto: bool,
    /// Delete confirmation, when present.
    pub delete: Option<Delete>,
    /// Whether a directory read or filesystem mutation is pending.
    pub busy: bool,
    /// Latest preview of the selected entry.
    pub preview: Preview,
}

impl FilePicker {
    /// Return the active directory of the completed listing.
    pub fn directory(&self) -> Option<&Path> {
        self.listing.as_ref()?.directory.as_deref()
    }

    /// Return the currently highlighted entry, if the filter has any matches.
    pub fn entry(&self) -> Option<&PickerEntry> {
        self.entry_at(self.selected)
    }

    /// Resolve a filtered row index to its entry.
    pub fn entry_at(&self, row: usize) -> Option<&PickerEntry> {
        self.listing.as_ref()?.entries.get(*self.visible.get(row)?)
    }

    /// Install a completed listing, keeping queries on a refresh of the same directory.
    pub fn install(&mut self, listing: Listing, keep_queries: bool) {
        let preferred = listing.selected.clone();
        self.listing = Some(listing);
        if !keep_queries {
            self.filter.clear();
            self.search.clear();
        }
        self.selected = 0;
        self.first = 0;
        self.refilter(preferred.as_deref());
    }

    /// Update live filter or search matches after editing the prompt.
    /// `preferred` is the selected path to retain when filtering changes its row index.
    pub fn update_prompt_matches(&mut self, preferred: Option<&Path>) {
        match self.prompt.as_ref().map(|prompt| &prompt.kind) {
            Some(PromptKind::Filter) => self.refilter(preferred),
            Some(PromptKind::Search) => self.refresh_search(),
            _ => {}
        }
    }

    /// End an applied filter or its input prompt and restore the full listing.
    /// Return whether the filter handled Escape.
    pub fn clear_filter(&mut self) -> bool {
        match &self.prompt {
            Some(prompt) if !matches!(prompt.kind, PromptKind::Filter) => return false,
            None if self.filter.is_empty() => return false,
            _ => {}
        }
        let preferred = self.entry().map(|entry| entry.path.clone());
        self.prompt = None;
        self.composition.clear();
        self.filter.clear();
        self.goto = false;
        self.refilter(preferred.as_deref());
        true
    }

    /// Rebuild matching indices, retaining the highlighted path when it still matches.
    pub fn refilter(&mut self, preferred: Option<&Path>) {
        let query = self
            .prompt
            .as_ref()
            .filter(|prompt| matches!(prompt.kind, PromptKind::Filter))
            .map_or(self.filter.as_str(), |prompt| prompt.value.as_str());
        self.visible.clear();
        self.selected = 0;
        if let Some(listing) = &self.listing {
            let matches = name_matches(query);
            for (index, entry) in listing.entries.iter().enumerate() {
                if matches(&entry.name()) {
                    if preferred.is_some_and(|path| same_row(&entry.path, path)) {
                        self.selected = self.visible.len();
                    }
                    self.visible.push(index);
                }
            }
        }
        self.refresh_search();
    }

    /// Refresh the highlighted rows within the visible list without moving the selection.
    fn refresh_search(&mut self) {
        let query = self
            .prompt
            .as_ref()
            .filter(|prompt| matches!(prompt.kind, PromptKind::Search))
            .map_or(self.search.as_str(), |prompt| prompt.value.as_str());
        self.search_rows.clear();
        if !query.is_empty()
            && let Some(listing) = &self.listing
        {
            let matches = name_matches(query);
            self.search_rows
                .extend(self.visible.iter().enumerate().filter_map(|(row, &index)| {
                    matches(&listing.entries[index].name()).then_some(row)
                }));
        }
    }

    /// Apply `query` and select the first match at or after the selection, wrapping at the end.
    /// Return whether a match was found.
    pub fn apply_search(&mut self, query: String) -> bool {
        self.search = query;
        self.refresh_search();
        let next = self.search_rows.partition_point(|&row| row < self.selected);
        let Some(&row) = self
            .search_rows
            .get(next)
            .or_else(|| self.search_rows.first())
        else {
            return false;
        };
        self.selected = row;
        true
    }

    /// Move to the next match, or the previous match when `backward`, wrapping at the ends.
    pub fn repeat_search(&mut self, backward: bool) {
        let Some(last) = self.search_rows.len().checked_sub(1) else {
            return;
        };
        let next = if backward {
            self.search_rows
                .partition_point(|&row| row < self.selected)
                .checked_sub(1)
                .unwrap_or(last)
        } else {
            self.search_rows
                .partition_point(|&row| row <= self.selected)
                % self.search_rows.len()
        };
        self.selected = self.search_rows[next];
    }

    /// Clear the committed search, returning whether it consumed Escape in the list.
    pub fn clear_search(&mut self) -> bool {
        if self.search.is_empty() {
            return false;
        }
        self.search.clear();
        self.search_rows.clear();
        self.goto = false;
        true
    }

    /// Keep the selected row in height rows and clamp after filtering or resize.
    pub fn reveal(&mut self, height: usize) {
        self.first = self
            .first
            .min(self.selected)
            .min(self.visible.len().saturating_sub(height));
        self.first = self.first.max(
            self.selected
                .saturating_add(1)
                .saturating_sub(height.max(1)),
        );
    }

    /// Scroll the right pane by rows within the available preview.
    pub fn scroll_preview(&mut self, rows: isize, height: usize) {
        let total = self
            .delete
            .as_ref()
            .map_or_else(|| self.preview.rows(), |delete| delete.paths.len());
        self.preview_first = self
            .preview_first
            .saturating_add_signed(rows)
            .min(total.saturating_sub(height));
    }
}

/// Resolve user input against the current directory or the user's home.
pub(in crate::grid) fn jump_path(value: &str, directory: Option<&Path>) -> Result<PathBuf> {
    let value = value.trim();
    ensure!(!value.is_empty(), i18n::text("picker.no_directory"));
    #[cfg(windows)]
    let drive =
        if value.len() == 2 && value.as_bytes()[0].is_ascii_alphabetic() && value.ends_with(':') {
            Some(format!("{value}\\"))
        } else {
            None
        };
    #[cfg(windows)]
    let value = drive.as_deref().unwrap_or(value);
    let path = Path::new(value);
    if let Ok(relative) = path.strip_prefix("~") {
        return Ok(dirs::home_dir()
            .context(i18n::text("picker.no_directory"))?
            .join(relative));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(directory
        .context(i18n::text("picker.no_directory"))?
        .join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::render::LabelStyle;
    use nanobug::i18n::Locale;

    /// Opening at a project lists its siblings and highlights the requested directory.
    #[test]
    fn reveals_project_directory_among_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap().join("projects");
        let project = parent.join("z-project");
        fs::create_dir_all(parent.join("a-project")).unwrap();
        fs::create_dir(&project).unwrap();

        let mut picker = FilePicker::default();
        picker.install(
            Listing::read(Browse::reveal_directory(&project), &[]).unwrap(),
            false,
        );

        assert_eq!(picker.directory(), Some(parent.as_path()));
        assert_eq!(picker.visible.len(), 2);
        let selected = picker.entry().unwrap();
        assert!(selected.directory);
        assert_eq!(selected.path, project);
    }

    /// Resolve a standalone file's parent and preferred row using the same path spelling.
    #[test]
    fn selects_file_in_a_canonicalized_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("sub")).unwrap();
        let directory = temp.path().join("sub/..");
        let file = directory.join("测试.txt");
        fs::write(&file, "sample").unwrap();

        let mut picker = FilePicker::default();
        picker.install(
            Listing::read(Browse::Path(directory, Some(file.clone())), &[]).unwrap(),
            false,
        );

        assert_eq!(
            picker.directory(),
            Some(temp.path().canonicalize().unwrap().as_path())
        );
        assert_eq!(picker.entry().unwrap().path, file.canonicalize().unwrap());
    }

    /// A drive root has a row in the drive list, while a UNC share root opens directly.
    #[cfg(windows)]
    #[test]
    fn reveals_drive_root_and_browses_unc_share() {
        let drive = Path::new(r"\\?\C:\");
        assert!(
            matches!(Browse::reveal_directory(drive), Browse::Drives(Some(path)) if path == drive)
        );
        let share = Path::new(r"\\?\UNC\server\share\");
        assert!(
            matches!(Browse::reveal_directory(share), Browse::Path(path, None) if path == share)
        );
    }

    /// Revealing a typed path and filtering preserve the highlighted file by path.
    #[test]
    fn reveals_typed_file_and_filters_only_one_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let file = root.join("测试.TXT");
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(&file, "sample").unwrap();
        fs::write(root.join("nested/hidden.txt"), "sample").unwrap();
        let mut picker = FilePicker::default();
        picker.install(
            Listing::read(Browse::Path(file.clone(), None), &[]).unwrap(),
            false,
        );
        assert_eq!(picker.entry().unwrap().path, file);
        assert_eq!(picker.visible.len(), 2);
        picker.filter = "txt".into();
        picker.refilter(Some(&file));
        assert_eq!(picker.visible.len(), 1);
        assert_eq!(picker.entry().unwrap().path, file);
        picker.filter = "TxT".into();
        picker.refilter(Some(&file));
        assert!(picker.entry().is_none());
    }

    /// File type restrictions apply to all three panes and survive clearing a text filter.
    #[test]
    fn lists_only_directories_and_matching_files_for_one_picker_session() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let current = root.join("project");
        let nested = current.join("nested.zip");
        fs::create_dir_all(&nested).unwrap();
        for directory in [&root, &current, &nested] {
            for file in ["测试.ZIP", "README.md", "archive.zip.txt"] {
                fs::write(directory.join(file), "").unwrap();
            }
        }

        let extensions = &["zip"];
        let listing = Listing::read(Browse::Path(current.clone(), None), extensions).unwrap();
        let names = |entries: &[PickerEntry]| {
            entries
                .iter()
                .map(|entry| entry.name().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&listing.parent), ["project", "测试.ZIP"]);
        assert_eq!(names(&listing.entries), ["nested.zip", "测试.ZIP"]);
        assert!(listing.entries[0].directory);
        let Preview::Directory(children) = Preview::read(&nested, true, extensions) else {
            panic!("expected a directory preview");
        };
        assert_eq!(names(&children), ["测试.ZIP"]);

        let mut picker = FilePicker::default();
        picker.install(listing, false);
        picker.filter = "README".into();
        picker.refilter(None);
        assert!(picker.entry().is_none());
        assert!(picker.clear_filter());
        assert_eq!(picker.visible.len(), 2);

        let ordinary = Listing::read(Browse::Path(current, None), &[]).unwrap();
        assert_eq!(
            names(&ordinary.entries),
            ["nested.zip", "archive.zip.txt", "README.md", "测试.ZIP"]
        );
    }

    /// Escape consumes either form of filtering once and preserves the highlighted file.
    #[test]
    fn clearing_filter_restores_listing_before_the_picker_can_close() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let selected = root.join("notes.txt");
        fs::write(&selected, "").unwrap();
        fs::write(root.join("draft.md"), "").unwrap();
        for editing in [false, true] {
            let mut picker = FilePicker::default();
            picker.install(
                Listing::read(Browse::Path(root.clone(), Some(selected.clone())), &[]).unwrap(),
                false,
            );
            picker.filter = "txt".into();
            if editing {
                picker.prompt = Some(Prompt::new(PromptKind::Filter, "notes".into(), 5));
            }
            picker.refilter(Some(&selected));
            assert_eq!(picker.visible.len(), 1);

            assert!(picker.clear_filter());
            assert!(picker.prompt.is_none());
            assert!(picker.filter.is_empty());
            assert_eq!(picker.visible.len(), 2);
            assert_eq!(picker.entry().unwrap().path, selected);
            assert!(!picker.clear_filter());
        }
    }

    /// Search follows visible row indices through filtering without changing filesystem marks.
    #[test]
    fn name_search_highlights_and_cycles_within_the_visible_list() {
        let root = PathBuf::from("docs");
        let paths = ["alpha.md", "beta.txt", "charlie.MD", "delta.md", "echo.txt"]
            .map(|name| root.join(name));
        let listing = || Listing {
            directory: Some(root.clone()),
            parent: Vec::new(),
            entries: paths
                .iter()
                .cloned()
                .map(|path| PickerEntry {
                    path,
                    directory: false,
                })
                .collect(),
            selected: Some(paths[1].clone()),
        };
        let mut picker = FilePicker::default();
        picker.install(listing(), false);
        picker.marked.insert(paths[1].clone());
        picker.prompt = Some(Prompt::new(PromptKind::Search, ".md".into(), 3));
        picker.update_prompt_matches(None);
        assert_eq!(picker.search_rows, [0, 2, 3]);
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.visible.len(), 5);

        let query = picker.prompt.take().unwrap().value;
        assert!(picker.apply_search(query));
        assert_eq!(picker.selected, 2);
        for (backward, expected) in [(false, 3), (false, 0), (true, 3), (true, 2)] {
            picker.repeat_search(backward);
            assert_eq!(picker.selected, expected);
        }
        assert_eq!(picker.marked, HashSet::from([paths[1].clone()]));

        picker.filter = ".md".into();
        picker.refilter(Some(&paths[2]));
        assert_eq!(picker.search_rows, [0, 1, 2]);
        assert!(picker.apply_search(".MD".into()));
        assert_eq!(picker.search_rows, [1]);
        assert_eq!(picker.entry().unwrap().path, paths[2]);
        picker.install(listing(), true);
        assert_eq!(picker.search_rows, [1]);
        assert!(picker.clear_search());
        assert!(picker.search_rows.is_empty());
        assert_eq!(picker.visible.len(), 3);
        assert!(!picker.clear_search());

        assert!(!picker.apply_search("missing".into()));
        let selection = picker.selected;
        picker.repeat_search(false);
        assert_eq!(picker.selected, selection);
        picker.install(listing(), false);
        assert!(picker.search.is_empty());
        assert_eq!(picker.visible.len(), 5);
    }

    /// Rename input replaces the selected stem and preserves a multibyte extension boundary.
    #[test]
    fn prompt_replaces_stem_and_deletes_whole_graphemes() {
        let mut prompt = Prompt::new(
            PromptKind::Rename(PathBuf::new()),
            "旧.txt".into(),
            "旧".len(),
        );
        prompt.insert("👩‍💻");
        assert_eq!(prompt.value, "👩‍💻.txt");
        assert!(prompt.edit("backspace"));
        assert_eq!(prompt.value, ".txt");
    }

    /// Verify the access key is the sole underlined span in the active catalog.
    #[test]
    fn delete_captions_share_keyboard_and_click_actions() {
        let captions = match i18n::locale() {
            Locale::English => ["[Yes]", "[No]"],
            Locale::SimplifiedChinese => ["[是(Y)]", "[否(N)]"],
        };
        for ((confirm, label), (caption, key)) in delete_buttons()
            .into_iter()
            .zip(captions.into_iter().zip(["y", "n"]))
        {
            assert_eq!(label.spans().map(|(text, _)| text).concat(), caption);
            let underlined: Vec<_> = label
                .spans()
                .into_iter()
                .filter(|(_, style)| *style == LabelStyle::Key)
                .collect();
            assert_eq!(underlined.len(), 1);
            assert!(underlined[0].0.eq_ignore_ascii_case(key));
            assert!(label.matches(key));
            assert_eq!(confirm, key == "y");
        }
    }

    /// Open a usable saved directory and use the fallback for missing or replaced locations.
    #[test]
    fn remembered_directory_falls_back_when_it_can_no_longer_be_browsed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let remembered = root.join("downloads");
        let fallback = root.join("project");
        fs::create_dir(&remembered).unwrap();
        fs::create_dir(&fallback).unwrap();
        fs::write(fallback.join("package.ZIP"), "fixture").unwrap();
        fs::write(fallback.join("README.md"), "fixture").unwrap();
        let read = || {
            Listing::read(
                Browse::RememberedDirectory(
                    remembered.clone(),
                    Box::new(Browse::Path(fallback.clone(), None)),
                ),
                &["zip"],
            )
            .unwrap()
        };

        let current = read();
        assert_eq!(current.directory.as_deref(), Some(remembered.as_path()));
        assert!(current.entries.is_empty());
        fs::remove_dir(&remembered).unwrap();
        assert_eq!(read().directory.as_deref(), Some(fallback.as_path()));
        fs::write(&remembered, "replaced directory").unwrap();
        let current = read();
        assert_eq!(current.directory.as_deref(), Some(fallback.as_path()));
        assert_eq!(current.entries.len(), 1);
        assert_eq!(current.entries[0].name(), "package.ZIP");
    }
}
