//! Filtered project history and the selected project's read-only directory preview.

use crate::{
    grid::{
        render::{Frame, Rect},
        windows::manager::Entry,
    },
    ui::{
        icons::Icon,
        text::TextSpacing,
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use gpui::KeyDownEvent;
use nanobug::i18n;
use std::{
    borrow::Cow,
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

/// Vertical offsets of the filter, separator and first project row.
const QUERY_ROW: usize = 1;
const RULE_ROW: usize = 2;
const PROJECT_TOP: usize = 3;
/// Starting column of the query and of the list cursor within its selected row.
const QUERY_COLUMN: usize = 3;
const PROJECT_CURSOR_COLUMN: usize = 1;
/// Caption column after the prompt symbol or folder icon.
const LABEL_COLUMN: usize = 4;
/// Plain folder icon for a saved project directory.
const FOLDER: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/start/folder.svg"),
    1.,
);

/// Borrowed directory scan result. None indicates a scan in progress for a selected project.
pub(super) type PreviewRows<'a> = Option<Result<&'a [Entry], &'a str>>;

/// Two adjacent panes sharing a title row and bottom margin.
#[derive(Clone, Copy)]
pub(in crate::grid) struct ProjectLayout {
    /// Complete picker bounds in the start-page grid.
    pub(super) area: Rect,
    /// Width of the left pane, including both border columns.
    left: usize,
}

impl ProjectLayout {
    /// Inset the picker from the edges of the `cols` by `rows` grid.
    pub(super) fn new(cols: usize, rows: usize) -> Self {
        let w = cols - 6;
        Self {
            area: Rect {
                x: 3,
                y: 1,
                w,
                h: rows - 2,
            },
            left: w / 2,
        }
    }

    /// Number of project rows below the filter and above the feedback line.
    pub(in crate::grid) fn project_rows(self) -> usize {
        self.area.h - PROJECT_TOP - 2
    }

    /// Number of directory entries visible between the right pane's borders.
    pub(in crate::grid) fn preview_rows(self) -> usize {
        self.area.h - 2
    }

    /// Whether the wheel at `(x, y)` is over the directory preview.
    pub(in crate::grid) fn over_preview(self, x: usize, y: usize) -> bool {
        self.area.contains(x, y) && x >= self.area.x + self.left
    }

    /// Whether the press at `(x, y)` is inside the filter prompt.
    pub(super) fn over_filter(self, x: usize, y: usize) -> bool {
        y == self.area.y + QUERY_ROW && (self.area.x + 1..self.area.x + self.left - 1).contains(&x)
    }
}

/// A saved directory with display and filter text derived from its native path.
struct ProjectChoice {
    /// Canonical directory used to open or preview this project.
    root: PathBuf,
    /// Visible directory basename, or the path for a volume root.
    name: String,
    /// Lowercase display name used for substring matching.
    search: String,
    /// Parent path shown beside duplicate names.
    parent: Option<String>,
}

/// Workspace action requested by a key in the project picker.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::grid) enum ProjectPickerAction {
    /// Refresh the picker and the selected directory preview.
    Update,
    /// Open the selected project.
    Open,
    /// Return to the launcher.
    Close,
    /// Leave the view as it is.
    Ignore,
}

/// Snapshot of project history and input state for one open picker.
pub(in crate::grid) struct ProjectPicker {
    /// Projects in history order.
    projects: Vec<ProjectChoice>,
    /// Indices of the projects satisfying the query, in history order.
    matches: Vec<usize>,
    /// Current ASCII filter input.
    query: String,
    /// Whether printable keys edit the query and the cursor belongs to the filter.
    filter_focused: bool,
    /// Decimal repeat count for the next `j` or `k` in the list.
    count: isize,
    /// Selection within the filtered list.
    selected: usize,
    /// First visible project when scrolling the filtered list.
    first: usize,
    /// First visible entry in the selected directory preview.
    preview_first: usize,
}

impl ProjectPicker {
    /// Start with all recent project `roots` visible and the filter ready for input.
    pub(in crate::grid) fn new(roots: &[PathBuf]) -> Self {
        let mut names = HashMap::new();
        let mut projects: Vec<_> = roots
            .iter()
            .map(|root| {
                let name = root
                    .file_name()
                    .map_or_else(|| display_path(root), |leaf| leaf.to_string_lossy())
                    .into_owned();
                let search = name.to_lowercase();
                *names.entry(search.clone()).or_insert(0) += 1;
                ProjectChoice {
                    root: root.clone(),
                    name,
                    search,
                    parent: None,
                }
            })
            .collect();
        for project in &mut projects {
            if names[&project.search] > 1 {
                project.parent = project
                    .root
                    .parent()
                    .map(|path| display_path(path).into_owned());
            }
        }
        Self {
            matches: (0..projects.len()).collect(),
            projects,
            query: String::new(),
            filter_focused: true,
            count: 0,
            selected: 0,
            first: 0,
            preview_first: 0,
        }
    }

    /// Return the selected index in the filtered list, if a project matches.
    pub(in crate::grid) fn selected(&self) -> Option<usize> {
        (self.selected < self.matches.len()).then_some(self.selected)
    }

    /// Whether the filter owns the cursor and accepts text.
    pub(super) fn filter_focused(&self) -> bool {
        self.filter_focused
    }

    /// Return the current caret's column and row relative to `layout`.
    pub(super) fn caret(&self, layout: ProjectLayout) -> (usize, usize) {
        if self.filter_focused() {
            let (query, _) = self.filter_row(layout.left);
            (QUERY_COLUMN + query.len(), QUERY_ROW)
        } else {
            let first = self.visible(layout.project_rows()).start;
            (PROJECT_CURSOR_COLUMN, PROJECT_TOP + self.selected - first)
        }
    }

    /// Give the filter the cursor and keep its current query.
    pub(in crate::grid) fn focus_filter(&mut self) {
        self.filter_focused = true;
        self.count = 0;
    }

    /// Focus the list while keeping the current query and selection.
    pub(in crate::grid) fn focus_list(&mut self) {
        self.filter_focused = false;
        self.count = 0;
    }

    /// Interpret `event` and its printable ASCII `character` in a list with `rows` visible rows.
    /// Return the workspace action to take after updating the query, focus or selection.
    pub(in crate::grid) fn key(
        &mut self,
        event: &KeyDownEvent,
        character: Option<&str>,
        rows: usize,
    ) -> ProjectPickerAction {
        use ProjectPickerAction::{Close, Ignore, Open, Update};

        let key = &event.keystroke;
        let raw = key.key.as_str();
        let modifiers = key.modifiers;
        if modifiers.alt || modifiers.platform || modifiers.function {
            self.count = 0;
            return Ignore;
        }
        // Held keys must not close the picker or type a slash after switching modes.
        if event.is_held && (matches!(raw, "escape" | "enter") || character == Some("/")) {
            return Ignore;
        }
        if !self.filter_focused
            && let Some([digit @ b'0'..=b'9']) = character.map(str::as_bytes)
        {
            self.count = self
                .count
                .saturating_mul(10)
                .saturating_add(isize::from(*digit - b'0'));
            return Ignore;
        }
        let count = std::mem::take(&mut self.count).max(1);
        match (modifiers.control, raw) {
            (false, "up") | (true, "k") => self.move_selection(-1, rows),
            (false, "down") | (true, "j") => self.move_selection(1, rows),
            (false, "pageup") => self.move_selection(-(rows as isize), rows),
            (false, "pagedown") => self.move_selection(rows as isize, rows),
            (false, "backspace") if self.filter_focused => self.erase(false),
            (true, "u") if self.filter_focused => self.erase(true),
            (false, "enter") => return Open,
            (false, "escape") if self.filter_focused => self.focus_list(),
            (false, "escape") => return Close,
            _ => match (self.filter_focused, character) {
                (true, Some(text)) => self.append(text),
                (false, Some("j")) => self.move_selection(count, rows),
                (false, Some("k")) => self.move_selection(-count, rows),
                (false, Some("/")) => self.focus_filter(),
                _ => return Ignore,
            },
        }
        Update
    }

    /// Return the native path of the selected project.
    pub(in crate::grid) fn selected_root(&self) -> Option<&Path> {
        self.selected_choice().map(|project| project.root.as_path())
    }

    /// Return the project record for the highlighted row.
    fn selected_choice(&self) -> Option<&ProjectChoice> {
        self.matches
            .get(self.selected)
            .map(|&index| &self.projects[index])
    }

    /// Append printable ASCII `text` and recompute the filtered list.
    fn append(&mut self, text: &str) {
        self.query.push_str(text);
        self.refilter();
    }

    /// Remove the last character from the filter or clear it when `all` is true.
    fn erase(&mut self, all: bool) {
        if self.query.is_empty() {
            return;
        }
        if all {
            self.query.clear();
        } else {
            self.query.pop();
        }
        self.refilter();
    }

    /// Recompute the matches and select the first row.
    fn refilter(&mut self) {
        let query = self.query.trim().to_ascii_lowercase();
        self.matches = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(index, project)| project.search.contains(&query).then_some(index))
            .collect();
        self.selected = 0;
        self.first = 0;
        self.preview_first = 0;
    }

    /// Move the selected project by `offset`, keeping it within `rows` visible items.
    pub(in crate::grid) fn move_selection(&mut self, offset: isize, rows: usize) {
        let index = self
            .selected
            .saturating_add_signed(offset)
            .min(self.matches.len().saturating_sub(1));
        self.select(index, rows);
    }

    /// Select filtered `index` and reveal it in `rows` while keeping the input focus.
    pub(in crate::grid) fn select(&mut self, index: usize, rows: usize) {
        self.count = 0;
        if index < self.matches.len() && index != self.selected {
            self.selected = index;
            self.first = self.visible(rows).start;
            self.preview_first = 0;
        }
    }

    /// Scroll the selected directory preview of `count` entries by `offset` within `rows`.
    pub(in crate::grid) fn scroll_preview(&mut self, offset: isize, count: usize, rows: usize) {
        let max = count.saturating_sub(rows);
        self.preview_first = self
            .preview_first
            .min(max)
            .saturating_add_signed(offset)
            .min(max);
    }

    /// Return the range of filtered indices fitting `rows` and containing the selection.
    fn visible(&self, rows: usize) -> Range<usize> {
        let first = self
            .first
            .min(self.selected)
            .min(self.matches.len().saturating_sub(rows))
            .max((self.selected + 1).saturating_sub(rows));
        first..(first + rows).min(self.matches.len())
    }

    /// Resolve a press in `layout` at `(x, y)` to a visible filtered project row.
    pub(super) fn hit(&self, layout: ProjectLayout, x: usize, y: usize) -> Option<usize> {
        let column = x.checked_sub(layout.area.x)?;
        if column == 0 || column >= layout.left - 1 {
            return None;
        }
        let range = self.visible(layout.project_rows());
        let index = range.start + y.checked_sub(layout.area.y + PROJECT_TOP)?;
        range.contains(&index).then_some(index)
    }

    /// Draw the picker at `layout`, using the selected `preview` and any feedback.
    pub(super) fn frame(
        &self,
        layout: ProjectLayout,
        preview: PreviewRows<'_>,
        status: &str,
        history_error: Option<&str>,
        spacing: &TextSpacing,
    ) -> Frame {
        let Rect { w, h, .. } = layout.area;
        let right = layout.left;
        let mut frame = Frame::new(w, h);
        pane_border(
            &mut frame,
            0..layout.left,
            h,
            i18n::text("start.recent"),
            ACCENT,
            spacing,
        );
        let title = self
            .selected_choice()
            .map_or_else(|| i18n::text("start.preview"), |project| &project.name);
        pane_border(&mut frame, right..w, h, title, MUTED, spacing);
        self.draw_filter(&mut frame, layout.left, spacing);
        let end = layout.left - 1;
        if self.matches.is_empty() {
            let message = if self.projects.is_empty() {
                "start.empty"
            } else {
                "start.no_projects_match"
            };
            frame.label(PROJECT_TOP, 2..end, i18n::text(message), MUTED, spacing);
        }
        for (row, index) in self.visible(layout.project_rows()).enumerate() {
            let row = PROJECT_TOP + row;
            let project = &self.projects[self.matches[index]];
            let color = if index == self.selected {
                frame.tint(row, 1, end, SELECTION);
                ACCENT
            } else {
                TEXT
            };
            frame.icon(2, row, FOLDER);
            let name_width = if project.parent.is_some() {
                (end - LABEL_COLUMN) / 2
            } else {
                end - LABEL_COLUMN
            };
            let name = spacing.ellipsize(&project.name, name_width);
            let after = frame.label(row, LABEL_COLUMN..end, &name, color, spacing);
            if let Some(parent) = &project.parent {
                let slot = after + 2..end;
                frame.label(
                    row,
                    slot.clone(),
                    &spacing.ellipsize(parent, slot.len()),
                    MUTED,
                    spacing,
                );
            }
        }
        let status = if status.is_empty()
            && let Some(error) = history_error
        {
            Cow::Owned(i18n::format("start.history_error", &[("error", &error)]))
        } else {
            Cow::Borrowed(status)
        };
        frame.label(
            h - 2,
            2..end,
            &spacing.ellipsize(&status, end - 2),
            GOLD,
            spacing,
        );
        if self.selected().is_some() {
            self.draw_preview(&mut frame, layout, preview, spacing);
        }
        frame
    }

    /// Fit the ASCII query and match count into the left pane of `width` columns.
    /// Keep a column after the visible query for the animated caret.
    fn filter_row(&self, width: usize) -> (&str, String) {
        let count = format!("{}/{}", self.matches.len(), self.projects.len());
        let available = width.saturating_sub(QUERY_COLUMN + count.len() + 4);
        let skip = self.query.len().saturating_sub(available);
        (&self.query[skip..], count)
    }

    /// Draw the filter prompt and count inside the left pane of `width` columns.
    fn draw_filter(&self, frame: &mut Frame, width: usize, spacing: &TextSpacing) {
        let (query, count) = self.filter_row(width);
        frame.right_label(QUERY_ROW, width - 2, &count, MUTED, spacing);
        frame.label(QUERY_ROW, 1..QUERY_COLUMN, "❯", ACCENT, spacing);
        frame.label(
            QUERY_ROW,
            QUERY_COLUMN..QUERY_COLUMN + query.len(),
            query,
            TEXT,
            spacing,
        );
        frame.text(1, RULE_ROW, &"─".repeat(width - 2), MUTED, width - 2);
    }

    /// Draw the current `preview` within the right pane of `layout`.
    fn draw_preview(
        &self,
        frame: &mut Frame,
        layout: ProjectLayout,
        preview: PreviewRows<'_>,
        spacing: &TextSpacing,
    ) {
        let x = layout.left;
        let slot = x + LABEL_COLUMN..layout.area.w - 1;
        match preview {
            Some(Ok(entries)) if !entries.is_empty() => {
                let visible = layout.preview_rows();
                let first = self
                    .preview_first
                    .min(entries.len().saturating_sub(visible));
                for (index, entry) in entries.iter().skip(first).take(visible).enumerate() {
                    let row = index + 1;
                    let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
                    frame.icon(x + 2, row, entry.icon(false));
                    frame.label(
                        row,
                        slot.clone(),
                        &spacing.ellipsize(&name, slot.len()),
                        TEXT,
                        spacing,
                    );
                }
            }
            Some(Err(error)) => {
                let message = i18n::format("start.preview_error", &[("error", &error)]);
                frame.label(
                    1,
                    slot.clone(),
                    &spacing.ellipsize(&message, slot.len()),
                    GOLD,
                    spacing,
                );
            }
            other => {
                let key = if other.is_some() {
                    "start.preview_empty"
                } else {
                    "start.preview_loading"
                };
                frame.label(1, slot, i18n::text(key), MUTED, spacing);
            }
        }
    }
}

/// Draw a thin frame in `color` around `columns` by `height`, measuring the centered `title` with `spacing`.
fn pane_border(
    frame: &mut Frame,
    columns: Range<usize>,
    height: usize,
    title: &str,
    color: u32,
    spacing: &TextSpacing,
) {
    let width = columns.len();
    let title = spacing.ellipsize(title, width - 6);
    let title_width = spacing.columns(&title);
    let left = (width - title_width) / 2;
    let right = width - left - title_width;
    let title_start = columns.start + left;
    frame.text(
        columns.start,
        0,
        &format!("╭{} ", "─".repeat(left - 2)),
        color,
        left,
    );
    frame.label(
        0,
        title_start..title_start + title_width,
        &title,
        color,
        spacing,
    );
    frame.text(
        title_start + title_width,
        0,
        &format!(" {}╮", "─".repeat(right - 2)),
        color,
        right,
    );
    for row in 1..height - 1 {
        frame.text(columns.start, row, "│", color, 1);
        frame.text(columns.end - 1, row, "│", color, 1);
    }
    frame.text(
        columns.start,
        height - 1,
        &format!("╰{}╯", "─".repeat(width - 2)),
        color,
        width,
    );
}

/// Show the user-facing spelling of a canonical `path`.
fn display_path(path: &Path) -> Cow<'_, str> {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(network) = text.strip_prefix(r"\\?\UNC\") {
            return Cow::Owned(format!(r"\\{network}"));
        }
        if let Some(disk) = text.strip_prefix(r"\\?\")
            && disk.as_bytes().get(1) == Some(&b':')
        {
            return Cow::Owned(disk.to_owned());
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::keys::normal_character;
    use gpui::Keystroke;

    /// Press `binding` in the `picker` at `layout` and return its requested action.
    fn press(
        picker: &mut ProjectPicker,
        layout: ProjectLayout,
        binding: &'static str,
    ) -> ProjectPickerAction {
        let keystroke = Keystroke::parse(binding).unwrap();
        let character = normal_character(&keystroke);
        picker.key(
            &KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
            },
            character.as_deref(),
            layout.project_rows(),
        )
    }

    /// Match contiguous directory-name text and move the caret with the input focus.
    #[test]
    fn filtering_selects_a_matching_project() {
        let roots = [PathBuf::from("Team/first"), PathBuf::from("Other/second")];
        let mut picker = ProjectPicker::new(&roots);
        let layout = ProjectLayout::new(90, 24);
        assert_eq!(picker.selected_root(), Some(roots[0].as_path()));
        assert_eq!(picker.caret(layout), (QUERY_COLUMN, QUERY_ROW));
        picker.append("OTHER");
        assert_eq!(picker.selected_root(), None);
        picker.erase(true);
        picker.append("SCD");
        assert_eq!(picker.selected_root(), None);
        picker.erase(true);
        picker.append(" EcO ");
        assert_eq!(picker.selected_root(), Some(roots[1].as_path()));
        assert_eq!(picker.caret(layout), (QUERY_COLUMN + 5, QUERY_ROW));
        picker.append(&"z".repeat(100));
        assert_eq!(picker.selected_root(), None);
        let (query, count) = picker.filter_row(layout.left);
        assert_eq!(
            picker.caret(layout),
            (QUERY_COLUMN + query.len(), QUERY_ROW)
        );
        assert_eq!(QUERY_COLUMN + query.len() + count.len() + 4, layout.left);
        assert_eq!(
            press(&mut picker, layout, "escape"),
            ProjectPickerAction::Update
        );
        assert_eq!(picker.caret(layout), (PROJECT_CURSOR_COLUMN, PROJECT_TOP));
        assert_eq!(picker.selected_root(), None);
        assert_eq!(press(&mut picker, layout, "/"), ProjectPickerAction::Update);
        picker.erase(true);
        assert_eq!(picker.selected_root(), Some(roots[0].as_path()));
        assert_eq!(picker.caret(layout), (QUERY_COLUMN, QUERY_ROW));
    }

    /// Escape keeps the active filter and selection, and a held Escape cannot close the picker.
    #[test]
    fn escape_leaves_filter_before_closing_picker() {
        use ProjectPickerAction::{Close, Ignore, Open, Update};

        let roots = [
            PathBuf::from("Team/jk2-first"),
            PathBuf::from("Team/jk2-second"),
        ];
        let mut picker = ProjectPicker::new(&roots);
        let layout = ProjectLayout::new(90, 24);
        for key in ["j", "k", "2", "down"] {
            assert_eq!(press(&mut picker, layout, key), Update);
        }
        assert_eq!(picker.caret(layout), (QUERY_COLUMN + 3, QUERY_ROW));
        assert_eq!(press(&mut picker, layout, "escape"), Update);
        assert_eq!(picker.query, "jk2");
        assert_eq!(picker.selected_root(), Some(roots[1].as_path()));
        assert_eq!(
            picker.caret(layout),
            (PROJECT_CURSOR_COLUMN, PROJECT_TOP + 1)
        );
        let held_escape = KeyDownEvent {
            keystroke: Keystroke::parse("escape").unwrap(),
            is_held: true,
            prefer_character_input: false,
        };
        assert_eq!(
            picker.key(&held_escape, None, layout.project_rows()),
            Ignore
        );
        assert_eq!(press(&mut picker, layout, "enter"), Open);
        assert_eq!(press(&mut picker, layout, "escape"), Close);
    }

    /// Counted motions reveal the target and preserve the mapping from visible rows to projects.
    #[test]
    fn project_hits_include_the_scrolled_offset() {
        let roots: Vec<_> = (0..40)
            .map(|i| PathBuf::from(format!("project-{i}")))
            .collect();
        let mut picker = ProjectPicker::new(&roots);
        let layout = ProjectLayout::new(90, 24);
        let rows = layout.project_rows();
        assert_eq!(
            press(&mut picker, layout, "escape"),
            ProjectPickerAction::Update
        );
        for key in ["4", "0", "j"] {
            press(&mut picker, layout, key);
        }
        assert_eq!(picker.selected(), Some(39));
        assert_eq!(
            picker.caret(layout),
            (PROJECT_CURSOR_COLUMN, PROJECT_TOP + rows - 1)
        );
        let last = layout.area.y + PROJECT_TOP + rows - 1;
        assert_eq!(picker.hit(layout, layout.area.x + 2, last), Some(39));
        assert_eq!(picker.hit(layout, layout.area.x + 2, last + 1), None);
        assert_eq!(
            picker.hit(layout, layout.area.x + layout.left + 2, last),
            None
        );
        for key in ["1", "2", "k"] {
            press(&mut picker, layout, key);
        }
        assert_eq!(picker.selected(), Some(27));
        press(&mut picker, layout, "j");
        assert_eq!(picker.selected(), Some(28));
    }
}
