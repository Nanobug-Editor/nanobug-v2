//! Directory navigation, text prompts, and selection state for the file picker.

mod preview;
mod view;

pub(in crate::grid) use preview::Preview;
pub(crate) use preview::PreviewHighlighter;
pub(in crate::grid) use view::PickerLayout;

use super::{Shortcut, ShortcutKeys::Fixed, manager::Explorer};
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

/// Bindings shown in the picker-specific help window.
pub(in crate::grid) const SHORTCUTS: &[Shortcut] = &[
    (Fixed("h / l  ← / →"), "picker.browse"),
    (Fixed("j / k  ↓ / ↑"), "picker.move"),
    (Fixed("gg / G"), "picker.ends"),
    (Fixed("g  gh  gd  g Space"), "picker.goto_help"),
    (Fixed("f"), "picker.filter_help"),
    (Fixed("Enter"), "picker.open_help"),
    (Fixed("O"), "picker.external_help"),
    (Fixed("J / K"), "picker.preview_scroll"),
    (Fixed("s"), "picker.mark_help"),
    (Fixed("y / c  x  v / p"), "picker.transfer_help"),
    (Fixed("a  r  d / D"), "picker.mutation_help"),
    (Fixed("q / Esc"), "picker.close_help"),
    (Fixed("Esc"), "picker.cancel_help"),
];

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
    /// List Windows volume roots, optionally selecting the one just left.
    #[cfg(windows)]
    Drives(Option<PathBuf>),
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
    /// Resolve request and scan its visible directories away from the UI thread.
    pub fn read(request: Browse) -> Result<Self> {
        let (path, selected) = match request {
            Browse::Path(path, selected) => (path, selected),
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
        let resolved = fs::canonicalize(&path).with_context(|| path.display().to_string())?;
        let (directory, selected) = if resolved.is_dir() {
            (resolved, selected)
        } else {
            let parent = resolved
                .parent()
                .context(i18n::text("picker.no_directory"))?;
            (parent.to_path_buf(), Some(resolved))
        };
        let entries = list(&directory)?;
        let parent = directory.parent().map_or_else(
            || {
                #[cfg(windows)]
                return drives();
                #[cfg(not(windows))]
                return Vec::new();
            },
            |parent| list(parent).unwrap_or_default(),
        );
        Ok(Self {
            directory: Some(directory),
            parent,
            entries,
            selected,
        })
    }
}

/// Enumerate one layer with Explorer's ordering, including navigable directory links.
fn list(path: &Path) -> Result<Vec<PickerEntry>> {
    Ok(Explorer::preview_with_directory_links(path.to_path_buf())?
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

/// Operation whose input is currently being edited.
pub(in crate::grid) enum PromptKind {
    /// Filter the current directory.
    Filter,
    /// Navigate to an absolute or directory-relative path.
    Jump,
    /// Create a child of the current directory.
    Create,
    /// Rename this selected path.
    Rename(PathBuf),
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
    /// Committed filter, used when the filter prompt is closed or canceled.
    pub filter: String,
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

    /// Install a completed listing and select its preferred child when visible.
    pub fn install(&mut self, listing: Listing, keep_filter: bool) {
        let preferred = listing.selected.clone();
        self.listing = Some(listing);
        if !keep_filter {
            self.filter.clear();
        }
        self.selected = 0;
        self.first = 0;
        self.refilter(preferred.as_deref());
    }

    /// Rebuild matching indices, retaining the highlighted path when it still matches.
    pub fn refilter(&mut self, preferred: Option<&Path>) {
        let query = self
            .prompt
            .as_ref()
            .filter(|prompt| matches!(prompt.kind, PromptKind::Filter))
            .map_or(self.filter.as_str(), |prompt| prompt.value.as_str());
        let exact_case = query.chars().any(char::is_uppercase);
        let folded = query.to_lowercase();
        self.visible.clear();
        self.selected = 0;
        if let Some(listing) = &self.listing {
            for (index, entry) in listing.entries.iter().enumerate() {
                let name = entry.name();
                if if exact_case {
                    name.contains(query)
                } else {
                    name.to_lowercase().contains(&folded)
                } {
                    if preferred.is_some_and(|path| same_row(&entry.path, path)) {
                        self.selected = self.visible.len();
                    }
                    self.visible.push(index);
                }
            }
        }
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
            Listing::read(Browse::Path(file.clone(), None)).unwrap(),
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
}
