//! Read-only start page with a launcher and a recent-project picker.

mod projects;

pub(in crate::grid) use projects::{ProjectLayout, ProjectPicker, ProjectPickerAction};

use crate::{
    grid::render::{CursorCell, Frame, Metrics, Rect},
    ui::{
        icons::Icon,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{GOLD, TEXT},
    },
};
use gpui::{Bounds, point, px, size};
use nanobug::i18n;

use self::projects::PreviewRows;

/// Pixel-art wordmark and mascot on the workspace material.
const LOGO: Icon = Icon::new(include_bytes!("../../../assets/icons/start-logo.svg"), 5.);
/// Wordmark width in columns and height in start-page rows.
const LOGO_COLUMNS: usize = 50;
const LOGO_ROWS: usize = 5;
/// Space between the wordmark and the first action.
const MENU_TOP: usize = LOGO_ROWS + 1;
/// Menu width and line pitch, matching the reference's 600-pixel span at 100% scale.
const MENU_COLUMNS: usize = 60;
const ROW_HEIGHT: f32 = 20.;
const MENU_STEP: usize = 2;
/// Start of the caption column, after the icon and its gutter.
const CAPTION_COLUMN: usize = 3;

/// A launcher command and the three fields displayed on its row.
struct LauncherItem {
    /// Command invoked by the row, key or Enter.
    action: StartAction,
    /// Fixed ASCII shortcut shown at the right edge.
    key: &'static str,
    /// Localized caption shown after the icon.
    caption: &'static str,
    /// Leftmost pictogram.
    icon: Icon,
}

/// Launcher rows in display order.
const ACTIONS: [LauncherItem; 5] = [
    LauncherItem {
        action: StartAction::File,
        key: "f",
        caption: "start.file",
        icon: Icon::new(
            include_bytes!("../../../assets/icons/start/find-file.svg"),
            1.,
        ),
    },
    LauncherItem {
        action: StartAction::Directory,
        key: "o",
        caption: "start.directory",
        icon: Icon::new(
            include_bytes!("../../../assets/icons/start/open-project.svg"),
            1.,
        ),
    },
    LauncherItem {
        action: StartAction::Projects,
        key: "p",
        caption: "start.projects",
        icon: Icon::new(
            include_bytes!("../../../assets/icons/start/projects.svg"),
            1.,
        ),
    },
    LauncherItem {
        action: StartAction::Config,
        key: "c",
        caption: "start.config",
        icon: Icon::new(include_bytes!("../../../assets/icons/start/config.svg"), 1.),
    },
    LauncherItem {
        action: StartAction::Quit,
        key: "q",
        caption: "start.quit",
        icon: Icon::new(include_bytes!("../../../assets/icons/start/quit.svg"), 1.),
    },
];

/// Command chosen from the start menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum StartAction {
    /// Show projects that have been opened before.
    Projects,
    /// Pick a directory in the native shell.
    Directory,
    /// Pick a file in the native shell.
    File,
    /// Report the availability of global configuration.
    Config,
    /// Close this start page.
    Quit,
}

impl StartAction {
    /// Match a native `key` against the ASCII shortcut on a row, ignoring case.
    pub(in crate::grid) fn from_key(key: &str) -> Option<Self> {
        ACTIONS
            .iter()
            .find(|item| item.key.eq_ignore_ascii_case(key))
            .map(|item| item.action)
    }
}

/// An item selected with the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum StartHit {
    /// Launcher command.
    Menu(StartAction),
    /// Row index within the filtered project list.
    Project(usize),
    /// Filter prompt in the project list.
    Filter,
}

/// The region of the start page that owns the shared cursor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::grid) enum StartFocus {
    /// Block cursor covering the first glyph of the selected launcher caption.
    Menu,
    /// Half-width cursor at the first column of the selected project or empty list.
    Projects,
    /// Thin cursor at the end of the visible project filter.
    Filter,
}

/// Per-tab navigation and feedback for the start page.
#[derive(Default)]
pub(in crate::grid) struct StartPage {
    /// Index of the launcher row with the block selection.
    menu_selection: usize,
    /// Picker shown in place of the launcher, with its own query and selection.
    pub(in crate::grid) picker: Option<Box<ProjectPicker>>,
    /// Last message from a launcher or project action.
    pub(in crate::grid) status: String,
}

/// Placement of the current start-page view.
#[derive(Clone, Copy)]
pub(in crate::grid) enum StartLayout {
    /// Centered logo and launcher.
    Menu(Rect),
    /// Full-height project list and directory preview.
    Projects(ProjectLayout),
}

impl StartPage {
    /// Use `grid`'s font and columns with the start page's shorter rows.
    pub(in crate::grid) fn content_metrics(grid: Metrics) -> Metrics {
        Metrics {
            lh: ROW_HEIGHT,
            ..grid
        }
    }

    /// Return the launcher command selected by the block caret.
    pub(in crate::grid) fn selected_action(&self) -> StartAction {
        ACTIONS[self.menu_selection].action
    }

    /// Move the launcher selection by `offset` within the displayed rows.
    pub(in crate::grid) fn move_menu(&mut self, offset: isize) {
        self.menu_selection = self
            .menu_selection
            .saturating_add_signed(offset)
            .min(ACTIONS.len() - 1);
    }

    /// Remember the launcher row activated by a shortcut or click.
    pub(in crate::grid) fn select_action(&mut self, action: StartAction) {
        self.menu_selection = ACTIONS
            .iter()
            .position(|item| item.action == action)
            .expect("launcher action has a row");
    }

    /// Identify the active cursor region for the launcher or picker input mode.
    pub(in crate::grid) fn cursor_focus(&self) -> StartFocus {
        match &self.picker {
            Some(picker) if picker.filter_focused() => StartFocus::Filter,
            Some(_) => StartFocus::Projects,
            None => StartFocus::Menu,
        }
    }

    /// Return the target cell in `layout` and its width in columns, measured with `spacing`.
    /// `metrics` are the start page's grid metrics and give the target's window position.
    pub(in crate::grid) fn caret(
        &self,
        layout: StartLayout,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> (CursorCell, f32) {
        let area = layout.area();
        let (column, row, width) = match layout {
            StartLayout::Menu(_) => {
                let caption = i18n::text(ACTIONS[self.menu_selection].caption);
                let width = spacing
                    .cells(caption)
                    .next()
                    .map_or(1., |cell| cell.end as f32 / UNITS_PER_COLUMN as f32);
                (
                    CAPTION_COLUMN,
                    MENU_TOP + self.menu_selection * MENU_STEP,
                    width,
                )
            }
            StartLayout::Projects(layout) => {
                let picker = self
                    .picker
                    .as_ref()
                    .expect("project layout belongs to the picker");
                let (column, row) = picker.caret(layout);
                (column, row, 1.)
            }
        };
        (
            CursorCell {
                row,
                column: column as f32,
                origin: metrics.at((area.x + column) as f32, (area.y + row) as f32),
            },
            width,
        )
    }

    /// Position the current view in `cols` by `rows`, below the native caption.
    pub(in crate::grid) fn layout(&self, cols: usize, rows: usize) -> StartLayout {
        if self.picker.is_some() {
            return StartLayout::Projects(ProjectLayout::new(cols, rows));
        }
        let w = cols.min(MENU_COLUMNS);
        let h = MENU_TOP + ACTIONS.len() * MENU_STEP + 1;
        StartLayout::Menu(Rect {
            x: (cols - w) / 2,
            y: 1 + rows.saturating_sub(h + 2) / 2,
            w,
            h,
        })
    }

    /// Draw the page at `layout`, with `preview` for the selected project and optional history feedback.
    pub(in crate::grid) fn frame(
        &self,
        layout: StartLayout,
        history_error: Option<&str>,
        preview: PreviewRows<'_>,
        metrics: Metrics,
        spacing: &TextSpacing,
    ) -> Frame {
        match layout {
            StartLayout::Menu(area) => self.menu_frame(area, metrics, spacing),
            StartLayout::Projects(area) => {
                let picker = self
                    .picker
                    .as_ref()
                    .expect("project layout belongs to the picker");
                picker.frame(area, preview, &self.status, history_error, spacing)
            }
        }
    }

    /// Draw the wordmark and the reference's icon, caption and shortcut columns in `area`.
    fn menu_frame(&self, area: Rect, metrics: Metrics, spacing: &TextSpacing) -> Frame {
        let mut frame = Frame::new(area.w, area.h);
        let logo_width = area.w.min(LOGO_COLUMNS);
        frame.image(
            (area.w - logo_width) / 2,
            0,
            LOGO,
            Bounds::new(
                point(px(0.), px(-metrics.lh / 2.)),
                size(
                    px(logo_width as f32 * metrics.cw),
                    px(LOGO_ROWS as f32 * metrics.lh),
                ),
            ),
        );
        for (index, item) in ACTIONS.iter().enumerate() {
            let row = MENU_TOP + index * MENU_STEP;
            frame.icon(0, row, item.icon);
            let caption = format!(
                "{:width$}{}",
                "",
                i18n::text(item.caption),
                width = CAPTION_COLUMN
            );
            frame.viewport_line(
                row,
                spacing.cells(&caption),
                0..area.w.saturating_sub(2),
                0,
                |_| (TEXT, 0),
            );
            frame.right_label(row, area.w, item.key, GOLD, spacing);
        }
        frame.label(area.h - 1, 0..area.w, &self.status, GOLD, spacing);
        frame
    }
}

impl StartLayout {
    /// Return the complete painted rectangle in start-page cells.
    pub(in crate::grid) fn area(self) -> Rect {
        match self {
            Self::Menu(area) => area,
            Self::Projects(layout) => layout.area,
        }
    }

    /// Identify the visible item pressed at `(x, y)` in `page`'s current view.
    pub(in crate::grid) fn hit(self, page: &StartPage, x: usize, y: usize) -> Option<StartHit> {
        let area = self.area();
        if !area.contains(x, y) {
            return None;
        }
        match self {
            Self::Projects(layout) if layout.over_filter(x, y) => Some(StartHit::Filter),
            Self::Projects(layout) => page
                .picker
                .as_ref()?
                .hit(layout, x, y)
                .map(StartHit::Project),
            Self::Menu(_) => {
                let item = (y - area.y).checked_sub(MENU_TOP)?;
                if !item.is_multiple_of(MENU_STEP) {
                    return None;
                }
                ACTIONS
                    .get(item / MENU_STEP)
                    .map(|row| StartHit::Menu(row.action))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clickable rows and ASCII shortcuts follow the same launcher order.
    #[test]
    fn menu_rows_follow_their_displayed_shortcuts() {
        let page = StartPage::default();
        let layout = page.layout(90, 24);
        let area = layout.area();
        assert_eq!(page.selected_action(), StartAction::File);
        assert_eq!(StartAction::from_key("n"), None);
        for (index, key) in ["f", "o", "p", "c", "q"].into_iter().enumerate() {
            let action = ACTIONS[index].action;
            assert_eq!(StartAction::from_key(key), Some(action));
            assert_eq!(StartAction::from_key(&key.to_uppercase()), Some(action));
            assert_eq!(
                layout.hit(&page, area.x + 3, area.y + MENU_TOP + index * MENU_STEP),
                Some(StartHit::Menu(action))
            );
        }
    }
}
