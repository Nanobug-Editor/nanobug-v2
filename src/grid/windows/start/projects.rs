//! Filtered project history and the selected project's read-only directory preview.

use crate::ui::components::grid_key_hints::GridKeyHints;
use crate::ui::components::grid_list::GridList;
use crate::ui::components::grid_text_field::{FieldLayout, GridTextField};
use crate::{
    grid::{
        render::{Frame, Metrics, OutlineStyle, Rect},
        windows::manager::Entry,
    },
    ui::{
        components::{grid_file_row::GridFileRow, grid_window::title_hit_columns},
        icons::Icon,
        ime::Composition,
        text::TextSpacing,
        theme::{ACCENT, GOLD, MUTED, SELECTION, TEXT},
    },
};
use gpui::{KeyDownEvent, Pixels, Point};
use nanobug::{
    animation::scroll::{GridScroll, ScrollInput},
    i18n,
};
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

/// Which panes remain visible in the open recent-project picker.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectPanes {
    Both,
    List,
    Preview,
}

/// The visible panes and their shared title row and bottom margin.
#[derive(Clone, Copy)]
pub(in crate::grid) struct ProjectLayout {
    /// Bounds of the bordered panes in the start-page grid.
    pub(super) area: Rect,
    panes: ProjectPanes,
}

impl ProjectLayout {
    /// Inset the picker from the edges of the `cols` by `rows` grid.
    fn new(cols: usize, rows: usize, panes: ProjectPanes) -> Self {
        Self {
            area: Rect {
                x: 3,
                y: 1,
                w: cols - 6,
                h: rows - 2,
            },
            panes,
        }
    }

    /// Painted bounds including the hint row below the panes.
    pub(super) fn frame_area(self) -> Rect {
        Rect {
            h: self.area.h + 1,
            ..self.area
        }
    }

    /// Bounds of the visible project list, which fills the picker by itself.
    fn list_area(self) -> Option<Rect> {
        match self.panes {
            ProjectPanes::Both => Some(Rect {
                w: self.area.w / 2,
                ..self.area
            }),
            ProjectPanes::List => Some(self.area),
            ProjectPanes::Preview => None,
        }
    }

    /// Bounds of the visible preview, separated from the list by one column when both are open.
    fn preview_area(self) -> Option<Rect> {
        match self.panes {
            ProjectPanes::Both => {
                let start = self.area.w / 2 + 1;
                Some(Rect {
                    x: self.area.x + start,
                    w: self.area.w - start,
                    ..self.area
                })
            }
            ProjectPanes::List => None,
            ProjectPanes::Preview => Some(self.area),
        }
    }

    /// Absolute start-grid bounds of the panes that are still visible.
    pub(in crate::grid) fn pane_areas(self) -> impl Iterator<Item = Rect> {
        [self.list_area(), self.preview_area()]
            .into_iter()
            .flatten()
    }

    /// Number of project rows between the filter and the status line.
    pub(in crate::grid) fn project_rows(self) -> usize {
        self.area.h - PROJECT_TOP - 2
    }

    /// Number of directory entries visible between the right pane's borders.
    pub(in crate::grid) fn preview_rows(self) -> usize {
        self.area.h - 2
    }

    /// Whether the wheel at `(x, y)` is over the directory preview.
    pub(in crate::grid) fn over_preview(self, x: usize, y: usize) -> bool {
        self.preview_area().is_some_and(|area| area.contains(x, y))
    }

    /// Whether the wheel at `(x, y)` is over the project list.
    pub(in crate::grid) fn over_list(self, x: usize, y: usize) -> bool {
        self.list_area().is_some_and(|area| area.contains(x, y))
    }

    /// Whether the press at `(x, y)` is inside the filter prompt.
    pub(super) fn over_filter(self, x: usize, y: usize) -> bool {
        self.list_area().is_some_and(|area| {
            y == area.y + QUERY_ROW && (area.x + 1..area.x + area.w - 1).contains(&x)
        })
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
    /// Ask whether to remove the selected project from history.
    ConfirmRemove,
    /// Let the native text handler process a key for the filter.
    Text,
    /// Return to the launcher.
    Close,
    /// Leave the view as it is.
    Ignore,
}

/// Snapshot of project history and input state for one open picker.
pub(in crate::grid) struct ProjectPicker {
    /// The pane titles still open in this picker session.
    panes: ProjectPanes,
    /// Projects in history order.
    projects: Vec<ProjectChoice>,
    /// Indices of the projects satisfying the query, in history order.
    matches: Vec<usize>,
    /// Committed filter text.
    query: String,
    field: GridTextField,
    /// Native preedit drawn after the committed filter text.
    pub(in crate::grid) composition: Composition,
    /// Whether printable keys edit the query and the cursor belongs to the filter.
    filter_focused: bool,
    /// Decimal repeat count for the next `j` or `k` in the list.
    count: isize,
    /// Selection within the filtered list.
    selected: usize,
    /// First visible project when scrolling the filtered list.
    list_scroll: GridScroll,
    /// First visible entry in the selected directory preview.
    preview_scroll: GridScroll,
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
            panes: ProjectPanes::Both,
            matches: (0..projects.len()).collect(),
            projects,
            query: String::new(),
            field: GridTextField::with_limit(usize::MAX),
            composition: Composition::default(),
            filter_focused: true,
            count: 0,
            selected: 0,
            list_scroll: GridScroll::default(),
            preview_scroll: GridScroll::default(),
        }
    }

    /// Load the latest `roots`, keeping the filter and selecting a nearby row if its project is gone.
    pub(in crate::grid) fn refresh_history(&mut self, roots: &[PathBuf]) {
        let mut refreshed = Self::new(roots);
        refreshed.panes = self.panes;
        refreshed.query = std::mem::take(&mut self.query);
        refreshed.field = std::mem::take(&mut self.field);
        refreshed.filter_focused = self.filter_focused;
        refreshed.refilter();
        let previous = self.selected_root();
        refreshed.selected = refreshed
            .matches
            .iter()
            .position(|&index| Some(refreshed.projects[index].root.as_path()) == previous)
            .unwrap_or(self.selected.min(refreshed.matches.len().saturating_sub(1)));
        let same_project = refreshed.selected_root() == previous;
        refreshed.list_scroll = std::mem::take(&mut self.list_scroll);
        if same_project {
            refreshed.preview_scroll = std::mem::take(&mut self.preview_scroll);
        }
        *self = refreshed;
    }

    /// Return the selected index in the filtered list, if a project matches.
    pub(in crate::grid) fn selected(&self) -> Option<usize> {
        (self.selected < self.matches.len()).then_some(self.selected)
    }

    /// Whether the filter owns the cursor and accepts text.
    pub(in crate::grid) fn filter_focused(&self) -> bool {
        self.filter_focused
    }

    /// Lay out the panes still open in the picker.
    pub(super) fn layout(&self, cols: usize, rows: usize) -> ProjectLayout {
        ProjectLayout::new(cols, rows, self.panes)
    }

    /// Whether the picker still shows the projects and filter.
    pub(super) fn shows_list(&self) -> bool {
        self.panes != ProjectPanes::Preview
    }

    /// Whether a scan is needed for the highlighted project's directory preview.
    pub(in crate::grid) fn shows_preview(&self) -> bool {
        self.panes != ProjectPanes::List
    }

    /// Close the title at `position`, or return to the launcher if it was the last pane.
    pub(in crate::grid) fn close_title_at(
        &mut self,
        layout: ProjectLayout,
        position: Point<Pixels>,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> ProjectPickerAction {
        let remaining = if layout.list_area().is_some_and(|area| {
            title_at(area, i18n::text("start.recent"), position, metrics, spacing)
        }) {
            ProjectPanes::Preview
        } else if layout
            .preview_area()
            .is_some_and(|area| title_at(area, self.preview_title(), position, metrics, spacing))
        {
            ProjectPanes::List
        } else {
            return ProjectPickerAction::Ignore;
        };
        if self.panes != ProjectPanes::Both {
            return ProjectPickerAction::Close;
        }
        self.panes = remaining;
        if !self.shows_list() {
            self.focus_list();
        }
        ProjectPickerAction::Update
    }

    /// Measure the caret's column with `spacing` and return its row relative to `layout`.
    pub(super) fn caret(&self, layout: ProjectLayout, spacing: &TextSpacing) -> (f32, usize) {
        if self.filter_focused() {
            let width = layout.list_area().expect("focused project list").w;
            let (field, _) = self.filter_row(width, spacing);
            (QUERY_COLUMN as f32 + field.caret_columns(), QUERY_ROW)
        } else {
            let first = self.visible(layout.project_rows()).range().start;
            (
                PROJECT_CURSOR_COLUMN as f32,
                PROJECT_TOP + self.selected.saturating_sub(first),
            )
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
        self.composition.clear();
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
        use ProjectPickerAction::{Close, ConfirmRemove, Ignore, Open, Text, Update};

        let key = &event.keystroke;
        let raw = key.key.as_str();
        let modifiers = key.modifiers;
        if self.filter_focused && !self.composition.is_empty() {
            return Text;
        }
        if modifiers.alt || modifiers.platform || modifiers.function {
            self.count = 0;
            return if self.filter_focused { Text } else { Ignore };
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
            (false, "up") | (true, "k") => self.move_selection(-1),
            (false, "down") | (true, "j") => self.move_selection(1),
            (false, "pageup") => self.move_selection(-(rows as isize)),
            (false, "pagedown") => self.move_selection(rows as isize),
            (true, "u") if self.filter_focused => self.clear_filter(),
            _ if self.filter_focused
                && let Some(changed) = self.field.edit(&mut self.query, key) =>
            {
                if changed {
                    self.refilter();
                }
            }
            (false, "enter") => return Open,
            (false, "escape") if self.filter_focused => self.focus_list(),
            (false, "escape") => return Close,
            _ => match (self.filter_focused, character) {
                (true, _) => return Text,
                (false, Some("j")) => self.move_selection(count),
                (false, Some("k")) => self.move_selection(-count),
                (false, Some("/")) if self.shows_list() => self.focus_filter(),
                (false, Some("d")) if !event.is_held && self.selected().is_some() => {
                    return ConfirmRemove;
                }
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

    /// Caption of the directory preview, even when no project matches the filter.
    fn preview_title(&self) -> &str {
        self.selected_choice()
            .map_or_else(|| i18n::text("start.preview"), |project| &project.name)
    }

    /// Replace the selected text with the committed single-line query and update matches.
    pub(in crate::grid) fn commit_filter(&mut self, text: &str) {
        let clean: String = text.chars().filter(|ch| !ch.is_control()).collect();
        self.field.insert(&mut self.query, &clean);
        self.refilter();
    }

    /// Clear the query and reset its selection and horizontal offset together.
    fn clear_filter(&mut self) {
        self.field.select_prefix(&self.query, self.query.len());
        self.field.insert(&mut self.query, "");
        self.refilter();
    }

    /// Recompute the matches and select the first row.
    fn refilter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.matches = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(index, project)| project.search.contains(&query).then_some(index))
            .collect();
        self.selected = 0;
        self.list_scroll = GridScroll::default();
        self.preview_scroll = GridScroll::default();
    }

    /// Move by a signed number of filtered projects, stopping at either end.
    pub(in crate::grid) fn move_selection(&mut self, offset: isize) {
        let index = GridList::moved(self.selected, offset, self.matches.len());
        self.select(index);
    }

    /// Select a filtered project and reset its directory preview while keeping the input focus.
    pub(in crate::grid) fn select(&mut self, index: usize) {
        self.count = 0;
        if index < self.matches.len() && index != self.selected {
            self.selected = index;
            self.preview_scroll = GridScroll::default();
        }
    }

    /// Advance project selection and the independently scrollable preview at workspace time.
    pub(in crate::grid) fn track(
        &mut self,
        rows: usize,
        preview_rows: usize,
        preview_count: usize,
        now: f64,
    ) {
        GridList::follow(
            &mut self.list_scroll,
            self.matches.len(),
            rows,
            self.selected,
            now,
        );
        self.preview_scroll.track_rows(
            ScrollInput::Advance,
            None,
            preview_rows,
            preview_count,
            now,
        );
    }
    pub(in crate::grid) fn body_viewports(
        &self,
        layout: ProjectLayout,
    ) -> [Option<(Rect, f32)>; 2] {
        let list = layout.list_area().map(|area| {
            (
                Rect {
                    x: area.x + 1,
                    y: area.y + PROJECT_TOP,
                    w: area.w - 2,
                    h: layout.project_rows(),
                },
                self.list_scroll.fraction(),
            )
        });
        let preview = layout
            .preview_area()
            .filter(|_| self.selected().is_some())
            .map(|area| {
                (
                    Rect {
                        x: area.x + 1,
                        y: area.y + 1,
                        w: area.w - 2,
                        h: layout.preview_rows(),
                    },
                    self.preview_scroll.fraction(),
                )
            });
        [list, preview]
    }

    pub(super) fn list_fraction(&self) -> f32 {
        if self.filter_focused {
            0.
        } else {
            self.list_scroll.fraction()
        }
    }
    pub(in crate::grid) fn scrolling(&self) -> bool {
        self.list_scroll.is_active() || self.preview_scroll.is_active()
    }

    pub(in crate::grid) fn scroll_preview(
        &mut self,
        offset: isize,
        count: usize,
        rows: usize,
        now: f64,
    ) {
        self.preview_scroll
            .track_rows(ScrollInput::Wheel(offset), None, rows, count, now);
    }

    fn visible(&self, rows: usize) -> GridList {
        GridList::scrolled(self.matches.len(), rows, self.list_scroll.position())
    }

    /// Resolve a press in `layout` at `(x, y)` to a visible filtered project row.
    pub(super) fn hit(&self, layout: ProjectLayout, x: usize, y: f32) -> Option<usize> {
        let area = layout.list_area()?;
        let column = x.checked_sub(area.x)?;
        if column == 0 || column >= area.w - 1 {
            return None;
        }
        self.visible(layout.project_rows())
            .hit(y - (layout.area.y + PROJECT_TOP) as f32)
    }

    /// Draw fixed pane titles, the filter and the footer. Scrolled bodies are composed separately.
    pub(super) fn frame(
        &self,
        layout: ProjectLayout,
        status: &str,
        history_error: Option<&str>,
        spacing: &TextSpacing,
    ) -> Frame {
        let Rect { w, h, .. } = layout.area;
        let mut frame = Frame::new(w, layout.frame_area().h);
        if let Some(list) = layout.list_area() {
            pane_border(
                &mut frame,
                0..list.w,
                h,
                i18n::text("start.recent"),
                ACCENT,
                spacing,
            );
            self.draw_filter(&mut frame, list.w, spacing);
        }
        if let Some(pane) = layout.preview_area() {
            let columns = pane.x - layout.area.x..w;
            pane_border(&mut frame, columns, h, self.preview_title(), MUTED, spacing);
        }
        let status = if status.is_empty()
            && let Some(error) = history_error
        {
            Cow::Owned(i18n::format("start.history_error", &[("error", &error)]))
        } else {
            Cow::Borrowed(status)
        };
        if status.is_empty() {
            let slot = 2..w - 2;
            let shortcuts = self.key_hints(slot.len(), spacing);
            frame.key_hints(h, slot.start, &shortcuts, (TEXT, TEXT), spacing);
        } else {
            let (row, end) = layout
                .list_area()
                .map_or((h, w - 1), |list| (h - 2, list.w - 1));
            frame.label(
                row,
                2..end,
                &spacing.ellipsize(&status, end - 2),
                GOLD,
                spacing,
            );
        }
        frame
    }

    /// Retained bodies have their own clips so fractional rows cannot cover the filter or the footer.
    pub(in crate::grid) fn bodies(
        &self,
        layout: ProjectLayout,
        preview: PreviewRows<'_>,
        spacing: &TextSpacing,
    ) -> [Option<Frame>; 2] {
        let [list, preview_area] = self.body_viewports(layout);
        let list = list.map(|(viewport, _)| {
            let rows = viewport.h;
            let mut frame = Frame::viewport(viewport.w, rows + 1, self.visible(rows).range().start);
            frame.hide_theme_background();
            self.draw_list(&mut frame, viewport.w, rows, spacing);
            frame
        });
        let preview = preview_area.map(|(viewport, _)| {
            let rows = viewport.h;
            let count = preview.and_then(Result::ok).map_or(0, <[Entry]>::len);
            let list = GridList::scrolled(count, rows, self.preview_scroll.position());
            let mut frame = Frame::viewport(viewport.w, rows + 1, list.range().start);
            frame.hide_theme_background();
            self.draw_preview(&mut frame, viewport.w, list, preview, spacing);
            frame
        });
        [list, preview]
    }

    /// Draw project names and icons relative to the scrolled list viewport.
    fn draw_list(&self, frame: &mut Frame, width: usize, rows: usize, spacing: &TextSpacing) {
        let end = width;
        if self.matches.is_empty() {
            let message = if self.projects.is_empty() {
                "start.empty"
            } else {
                "start.no_projects_match"
            };
            frame.label(0, 1..end, i18n::text(message), MUTED, spacing);
        }
        for (row, index) in self.visible(rows).range().enumerate() {
            let project = &self.projects[self.matches[index]];
            let color = if index == self.selected {
                frame.tint(row, 0, end, SELECTION);
                ACCENT
            } else {
                TEXT
            };
            frame.icon(1, row, FOLDER);
            let name_width = if project.parent.is_some() {
                (end.saturating_sub(LABEL_COLUMN - 1)) / 2
            } else {
                end.saturating_sub(LABEL_COLUMN - 1)
            };
            let name = spacing.ellipsize(&project.name, name_width);
            let after = frame.label(row, LABEL_COLUMN - 1..end, &name, color, spacing);
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
    }

    /// Format complete shortcuts for the current focus and selection within `columns`, measured by `spacing`.
    fn key_hints(&self, columns: usize, spacing: &TextSpacing) -> GridKeyHints {
        let focus_hints: &[(&str, &str)] = if !self.shows_list() {
            &[("picker.hint_back", "Esc")]
        } else if self.filter_focused() {
            &[("start.browse_projects", "Esc")]
        } else {
            &[("start.continue_filter", "/"), ("picker.hint_back", "Esc")]
        };
        let selection_hints: &[(&str, &str)] = match (self.selected(), self.filter_focused()) {
            (None, _) => &[],
            (Some(_), true) => &[("picker.hint_move", "↑/↓"), ("picker.hint_open", "Enter")],
            (Some(_), false) => &[
                ("picker.hint_move", "j/k"),
                ("picker.hint_open", "Enter"),
                ("start.hint_remove_entry", "d"),
            ],
        };
        GridKeyHints::fit(
            columns,
            focus_hints
                .iter()
                .chain(selection_hints)
                .map(|&(action, key)| (i18n::text(action), key)),
            spacing,
        )
    }

    /// Fit the query, preedit and count in `width` columns measured by `spacing`.
    /// Keep one column after the visible text for the caret.
    fn filter_row(&self, width: usize, spacing: &TextSpacing) -> (FieldLayout<'_>, String) {
        let count = format!("{}/{}", self.matches.len(), self.projects.len());
        let columns = width.saturating_sub(QUERY_COLUMN + count.len() + 3);
        (
            self.field
                .layout(&self.query, &self.composition, columns, spacing),
            count,
        )
    }

    /// Position the filter caret using the same clipped graphemes as its grid row.
    pub(in crate::grid) fn press_filter(
        &mut self,
        layout: ProjectLayout,
        point: Point<Pixels>,
        metrics: Metrics,
        spacing: &TextSpacing,
        extend: bool,
    ) {
        self.focus_filter();
        if !self.composition.is_empty() {
            return;
        }
        if let Some(area) = layout.list_area() {
            let x =
                f32::from(point.x - metrics.at((area.x + QUERY_COLUMN) as f32, 0.).x) / metrics.cw;
            let byte = self.filter_row(area.w, spacing).0.byte_at(x, spacing);
            self.field.place(&self.query, byte, extend);
        }
    }

    /// Draw the query and match count inside the left pane.
    fn draw_filter(&self, frame: &mut Frame, width: usize, spacing: &TextSpacing) {
        let (field, count) = self.filter_row(width, spacing);
        frame.right_label(QUERY_ROW, width - 2, &count, MUTED, spacing);
        frame.label(QUERY_ROW, 1..QUERY_COLUMN, "❯", ACCENT, spacing);
        frame.text_field(
            QUERY_ROW,
            QUERY_COLUMN..width.saturating_sub(count.len() + 3),
            &field,
            spacing,
        );
        frame.text(1, RULE_ROW, &"─".repeat(width - 2), MUTED, width - 2);
    }

    /// Draw the current `preview` within the pane's `columns` and visible `rows`.
    fn draw_preview(
        &self,
        frame: &mut Frame,
        width: usize,
        list: GridList,
        preview: PreviewRows<'_>,
        spacing: &TextSpacing,
    ) {
        let slot = LABEL_COLUMN - 1..width;
        match preview {
            Some(Ok(entries)) if !entries.is_empty() => {
                for (row, entry) in entries[list.range()].iter().enumerate() {
                    let mut file = GridFileRow::new(&entry.path, entry.directory, false, 0..width);
                    file.name_columns = slot.clone();
                    frame.file_row(row, &file, spacing, None);
                }
            }
            Some(Err(error)) => {
                let message = i18n::format("start.preview_error", &[("error", &error)]);
                frame.label(
                    0,
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
                frame.label(0, slot, i18n::text(key), MUTED, spacing);
            }
        }
    }
}

/// Draw the pane outline on `frame` across `columns` and `height` in `color`.
/// Reserve a gap for the centered `title` measured with `spacing`.
fn pane_border(
    frame: &mut Frame,
    columns: Range<usize>,
    height: usize,
    title: &str,
    color: u32,
    spacing: &TextSpacing,
) {
    let (title, slot) = pane_title(columns.len(), title, spacing);
    let title_columns = columns.start + slot.start..columns.start + slot.end;
    frame.outline(
        Rect {
            x: columns.start,
            y: 0,
            w: columns.len(),
            h: height,
        },
        color,
        OutlineStyle::Solid,
        std::iter::once(title_hit_columns(title_columns.clone())),
    );
    frame.label(0, title_columns, &title, color, spacing);
}

/// Center `title` in the same columns for painting and pointer hit testing.
fn pane_title<'a>(
    width: usize,
    title: &'a str,
    spacing: &TextSpacing,
) -> (Cow<'a, str>, Range<usize>) {
    let title = spacing.ellipsize(title, width.saturating_sub(6));
    let title_width = spacing.columns(&title);
    let start = (width - title_width) / 2;
    (title, start..start + title_width)
}

/// Test `position` against the title and its surrounding padding inside `area`.
fn title_at(
    area: Rect,
    title: &str,
    position: Point<Pixels>,
    metrics: Metrics,
    spacing: &TextSpacing,
) -> bool {
    let (_, columns) = pane_title(area.w, title, spacing);
    let columns = title_hit_columns(columns);
    !columns.is_empty()
        && Rect {
            x: area.x + columns.start,
            w: columns.len(),
            h: 1,
            ..area
        }
        .pixels(metrics)
        .contains(&position)
}

/// Show the user-facing spelling of a canonical `path`.
pub(super) fn display_path(path: &Path) -> Cow<'_, str> {
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
    use gpui::{FontId, Keystroke, NoopTextSystem, point, px};
    use std::sync::Arc;

    /// Give tests fixed-width ASCII and double-width Han spacing without a native window.
    fn spacing() -> TextSpacing {
        TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 10.)
    }

    /// Press `binding` in the `picker` at `layout`, committing printable native text when requested.
    fn press(
        picker: &mut ProjectPicker,
        layout: ProjectLayout,
        binding: &'static str,
    ) -> ProjectPickerAction {
        let keystroke = Keystroke::parse(binding).unwrap();
        let character = normal_character(&keystroke);
        let action = picker.key(
            &KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
            },
            character.as_deref(),
            layout.project_rows(),
        );
        if action == ProjectPickerAction::Text
            && let Some(text) = character
        {
            picker.commit_filter(&text);
            ProjectPickerAction::Update
        } else {
            action
        }
    }

    /// After scrolling, the preview keeps the icon and clipped name inside its own pane.
    #[test]
    fn scrolled_project_preview_uses_the_file_row_inside_its_border() -> anyhow::Result<()> {
        use crate::{grid::windows::manager::Explorer, ui::file_icons::EntryIcon};
        let project = tempfile::tempdir()?;
        let path = project.path().join("readme-long.txt");
        std::fs::create_dir(project.path().join("src"))?;
        std::fs::write(&path, "")?;
        let root = project.path().to_path_buf();
        let entries = Explorer::preview(root.clone())?;
        let mut picker = ProjectPicker::new(&[root]);
        picker.scroll_preview(1, entries.len(), 1, 0.);
        picker.track(1, 1, entries.len(), 1.);
        let spacing = spacing();
        let list = GridList::scrolled(entries.len(), 1, picker.preview_scroll.position());
        let mut preview = Frame::new(9, 1);
        picker.draw_preview(&mut preview, 9, list, Some(Ok(&entries)), &spacing);
        let mut expected = Frame::new(9, 1);
        expected.icon(1, 0, EntryIcon::for_path(&path, false, false).image());
        expected.label(0, 3..9, "readm…", TEXT, &spacing);
        assert!(preview == expected);
        Ok(())
    }

    /// Rendering and preview-wheel targeting share the same pane after the one-column gap.
    #[test]
    fn project_layout_reserves_a_column_between_panes() {
        let layout = ProjectLayout::new(90, 24, ProjectPanes::Both);
        let left = layout.list_area().unwrap();
        let right = layout.preview_area().unwrap();
        let gap = left.x + left.w;
        assert_eq!(right.x, gap + 1);
        assert_eq!(right.x + right.w, layout.area.x + layout.area.w);
        assert!(!layout.over_preview(gap, right.y + 1));
        assert!(layout.over_preview(right.x, right.y + 1));
    }

    /// Dismiss either visible caption independently and close on the last title.
    #[test]
    fn project_titles_close_independently() {
        use ProjectPickerAction::{Close, Ignore, Open, Update};

        let roots = [PathBuf::from("Team/project")];
        let metrics = Metrics {
            origin: point(px(0.), px(0.)),
            cw: 10.,
            lh: 20.,
            font_size: 16.,
        };
        let spacing = spacing();
        let center =
            |area: Rect| metrics.at(area.x as f32 + area.w as f32 / 2., area.y as f32 + 0.5);
        for close_list_first in [true, false] {
            let mut picker = ProjectPicker::new(&roots);
            let layout = picker.layout(90, 24);
            let first = if close_list_first {
                layout.list_area()
            } else {
                layout.preview_area()
            }
            .unwrap();
            let border = metrics.at(first.x as f32 + 0.5, first.y as f32 + 0.5);
            assert_eq!(
                picker.close_title_at(layout, border, metrics, &spacing),
                Ignore
            );
            assert_eq!(
                picker.close_title_at(layout, center(first), metrics, &spacing),
                Update
            );

            let layout = picker.layout(90, 24);
            assert!(layout.pane_areas().eq([layout.area]));
            assert_eq!(picker.shows_list(), !close_list_first);
            assert_eq!(picker.shows_preview(), close_list_first);
            assert_eq!(picker.filter_focused(), !close_list_first);
            assert_eq!(press(&mut picker, layout, "enter"), Open);
            if close_list_first {
                assert_eq!(press(&mut picker, layout, "/"), Ignore);
                assert!(!layout.over_filter(layout.area.x + 2, layout.area.y + QUERY_ROW));
            }
            picker.refresh_history(&roots);
            assert!(picker.layout(90, 24).pane_areas().eq([layout.area]));
            assert_eq!(
                picker.close_title_at(layout, center(layout.area), metrics, &spacing),
                Close
            );
        }
    }

    /// Match contiguous directory-name text and move the caret with the input focus.
    #[test]
    fn filtering_selects_a_matching_project() {
        let roots = [
            PathBuf::from("Team/first"),
            PathBuf::from("Other/second"),
            PathBuf::from("Team/项目示例"),
        ];
        let mut picker = ProjectPicker::new(&roots);
        let layout = picker.layout(90, 24);
        let width = layout.list_area().unwrap().w;
        let spacing = spacing();
        assert_eq!(picker.selected_root(), Some(roots[0].as_path()));
        assert_eq!(
            picker.caret(layout, &spacing),
            (QUERY_COLUMN as f32, QUERY_ROW)
        );
        picker.commit_filter("OTHER");
        assert_eq!(picker.selected_root(), None);
        picker.clear_filter();
        picker.commit_filter("SCD");
        assert_eq!(picker.selected_root(), None);
        picker.clear_filter();
        picker.commit_filter(" EcO ");
        assert_eq!(picker.selected_root(), Some(roots[1].as_path()));
        assert_eq!(
            picker.caret(layout, &spacing),
            ((QUERY_COLUMN + 5) as f32, QUERY_ROW)
        );
        picker.commit_filter(&"z".repeat(100));
        assert_eq!(picker.selected_root(), None);
        let (query, count) = picker.filter_row(width, &spacing);
        assert_eq!(
            picker.caret(layout, &spacing),
            ((QUERY_COLUMN + query.text().len()) as f32, QUERY_ROW)
        );
        assert_eq!(QUERY_COLUMN + query.text().len() + count.len() + 4, width);
        assert_eq!(
            press(&mut picker, layout, "escape"),
            ProjectPickerAction::Update
        );
        assert_eq!(
            picker.caret(layout, &spacing),
            (PROJECT_CURSOR_COLUMN as f32, PROJECT_TOP)
        );
        assert_eq!(picker.selected_root(), None);
        assert_eq!(press(&mut picker, layout, "/"), ProjectPickerAction::Update);
        picker.clear_filter();
        assert_eq!(picker.selected_root(), Some(roots[0].as_path()));
        assert_eq!(
            picker.caret(layout, &spacing),
            (QUERY_COLUMN as f32, QUERY_ROW)
        );
        picker.composition.mark(None, "项目", None);
        assert_eq!(picker.filter_row(width, &spacing).0.text(), "项目");
        assert_eq!(picker.selected_root(), Some(roots[0].as_path()));
        picker.focus_list();
        assert!(picker.composition.is_empty());
        press(&mut picker, layout, "/");
        picker.commit_filter("项目");
        assert_eq!(picker.selected_root(), Some(roots[2].as_path()));
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
        let layout = picker.layout(90, 24);
        let spacing = spacing();
        for key in ["j", "k", "2", "down"] {
            assert_eq!(press(&mut picker, layout, key), Update);
        }
        assert_eq!(
            picker.caret(layout, &spacing),
            ((QUERY_COLUMN + 3) as f32, QUERY_ROW)
        );
        assert_eq!(press(&mut picker, layout, "escape"), Update);
        assert_eq!(picker.query, "jk2");
        assert_eq!(picker.selected_root(), Some(roots[1].as_path()));
        assert_eq!(
            picker.caret(layout, &spacing),
            (PROJECT_CURSOR_COLUMN as f32, PROJECT_TOP + 1)
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

    /// Change the hint with the input mode while giving feedback priority.
    #[test]
    fn filter_hint_follows_list_focus() {
        let mut picker = ProjectPicker::new(&[]);
        let layout = picker.layout(90, 24);
        let spacing = spacing();
        let filtering = picker.frame(layout, "", None, &spacing);
        let status = "Project unavailable";
        let feedback = picker.frame(layout, status, None, &spacing);

        press(&mut picker, layout, "escape");
        assert!(picker.frame(layout, "", None, &spacing) != filtering);
        assert!(picker.frame(layout, status, None, &spacing) == feedback);

        press(&mut picker, layout, "/");
        assert!(picker.frame(layout, "", None, &spacing) == filtering);
    }

    /// Render mode-specific shortcut groups without cutting a label or its separator.
    #[test]
    fn shortcut_text_fits_complete_groups() {
        let roots = [PathBuf::from("Team/project")];
        let mut picker = ProjectPicker::new(&roots);
        let spacing = spacing();
        let browse = i18n::text("start.browse_projects");
        let movement = i18n::text("picker.hint_move");
        let open = i18n::text("picker.hint_open");
        let first = format!("{browse}: Esc");
        assert_eq!(
            picker.key_hints(120, &spacing).labels.join(" | "),
            format!("{first} | {movement}: ↑/↓ | {open}: Enter")
        );
        assert_eq!(
            picker
                .key_hints(spacing.columns(&first) + 3, &spacing)
                .labels
                .join(" | "),
            first
        );

        picker.focus_list();
        let filter = i18n::text("start.continue_filter");
        let back = i18n::text("picker.hint_back");
        let remove = i18n::text("start.hint_remove_entry");
        assert_eq!(
            picker.key_hints(120, &spacing).labels.join(" | "),
            format!("{filter}: / | {back}: Esc | {movement}: j/k | {open}: Enter | {remove}: d")
        );
    }

    /// Only list navigation requests removal, and history changes keep the filter and reveal a match.
    #[test]
    fn remove_key_and_history_refresh_keep_the_filter() {
        use ProjectPickerAction::{ConfirmRemove, Ignore, Update};

        let roots = [
            PathBuf::from("Team/delta"),
            PathBuf::from("Other/delta"),
            PathBuf::from("Team/first"),
        ];
        let mut picker = ProjectPicker::new(&roots);
        let layout = picker.layout(90, 24);
        assert_eq!(press(&mut picker, layout, "d"), Update);
        assert_eq!(picker.query, "d");
        assert_eq!(press(&mut picker, layout, "escape"), Update);
        assert_eq!(press(&mut picker, layout, "d"), ConfirmRemove);
        picker.refresh_history(&roots[1..]);
        assert_eq!(picker.query, "d");
        assert!(!picker.filter_focused());
        assert_eq!(picker.selected_root(), Some(roots[1].as_path()));
        assert!(picker.selected_choice().unwrap().parent.is_none());
        picker.refresh_history(&roots[2..]);
        assert_eq!(picker.selected_root(), None);
        assert_eq!(press(&mut picker, layout, "d"), Ignore);
    }

    /// Counted motions reveal the target and preserve the mapping from visible rows to projects.
    #[test]
    fn project_hits_include_the_scrolled_offset() {
        let roots: Vec<_> = (0..40)
            .map(|i| PathBuf::from(format!("project-{i}")))
            .collect();
        let mut picker = ProjectPicker::new(&roots);
        let layout = picker.layout(90, 24);
        let rows = layout.project_rows();
        assert_eq!(
            press(&mut picker, layout, "escape"),
            ProjectPickerAction::Update
        );
        for key in ["4", "0", "j"] {
            press(&mut picker, layout, key);
        }
        picker.track(rows, layout.preview_rows(), 0, 1.);
        assert_eq!(picker.selected(), Some(39));
        assert_eq!(
            picker.caret(layout, &spacing()),
            (PROJECT_CURSOR_COLUMN as f32, PROJECT_TOP + rows - 1)
        );
        let last = layout.area.y + PROJECT_TOP + rows - 1;
        assert_eq!(picker.hit(layout, layout.area.x + 2, last as f32), Some(39));
        assert_eq!(
            picker.hit(layout, layout.area.x + 2, (last + 1) as f32),
            None
        );
        assert_eq!(
            picker.hit(layout, layout.preview_area().unwrap().x + 1, last as f32),
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
