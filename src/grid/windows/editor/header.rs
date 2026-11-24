//! Lay out the current page's buffer labels for painting and pointer hit testing.

use super::EditorWindow;
use crate::ui::components::grid_tab_bar::{GridTabBar, TabAlign, TabGrowth, TabKind, TabOptions};
use crate::{
    grid::render::{Frame, Rect, SHORTCUT_SEPARATOR},
    ui::{
        file_icons::EntryIcon,
        text::TextSpacing,
        theme::{ACCENT, MUTED, TEXT},
    },
};
use nanobug::i18n;
use std::{ops::Range, path::Path};

/// Grid columns for the file icon and its padding.
const ICON_COLUMNS: usize = 2;
/// Blank columns beside each file label that also accept clicks.
const LABEL_HIT_PADDING: usize = 1;
/// Suffix for an edited buffer.
const MODIFIED: &str = " [+]";

/// Pointer actions associated with the visible labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum HeaderTarget {
    /// A file label, identified by its stable buffer ID.
    Buffer(u64),
    /// A buffer overflow indicator that jumps to the adjacent hidden buffer.
    Overflow(u64),
}

/// One buffer and its text before fitting the file list to the header.
struct Entry {
    /// File name displayed on the border.
    label: String,
    /// Stable identity of this buffer.
    id: u64,
    /// Optional file-type artwork.
    icon: Option<EntryIcon>,
    /// Whether this is the current buffer.
    active: bool,
    /// Whether the file should display a modified marker.
    modified: bool,
}

impl Entry {
    /// Measure the complete label, including the icon and marker when present.
    fn width(&self, spacing: &TextSpacing) -> usize {
        spacing.columns(&self.label)
            + usize::from(self.icon.is_some()) * ICON_COLUMNS
            + usize::from(self.modified) * spacing.columns(MODIFIED)
    }
}

/// One measured segment of the title, with its local hit interval if interactive.
struct Segment {
    /// Local columns, including any icon and modifier.
    columns: Range<usize>,
    /// Visible text, fitted to the assigned columns.
    label: String,
    /// Pointer action and the local columns that accept it, including any blank padding.
    hit: Option<(HeaderTarget, Range<usize>)>,
    /// File icon if its required space fits.
    icon: Option<EntryIcon>,
    /// Whether this is the selected buffer.
    active: bool,
}

/// Editor header geometry for one grid rectangle.
pub(in crate::grid) struct EditorHeader {
    /// Screen-relative title row in grid cells.
    title: Rect,
    /// File labels and their separators in left-to-right order.
    buffers: Vec<Segment>,
}

impl EditorWindow {
    /// Fit the active page's file labels to the editor border in `area`.
    /// Measure labels with `spacing` and clip to the `visible_cols` edge.
    pub(in crate::grid) fn header(
        &self,
        area: Rect,
        spacing: &TextSpacing,
        visible_cols: usize,
    ) -> EditorHeader {
        let title = Self::grid_window(area).title();
        let visible = Rect {
            w: title.w.min(visible_cols.saturating_sub(title.x)),
            ..title
        };
        let columns = visible.border_title_columns(None);
        let active = self.buffer().id;
        let buffers = self.visible_buffers().map(|buffer| {
            let doc = &buffer.vim.doc;
            let path = doc.path.as_deref();
            let mut label = path
                .and_then(Path::file_name)
                .map_or_else(
                    || i18n::text("editor.untitled").into(),
                    |name| name.to_string_lossy().into_owned(),
                )
                .replace(['\r', '\n', '\t'], " ");
            if buffer.terminal.is_some() {
                label = i18n::text("terminal.title").into();
            } else if let Some((list, _)) = buffer.list_owner() {
                label = list.title().into();
            } else if buffer.vim.is_help() && path.is_none() {
                label = i18n::text("vim_g.help_title").into();
            }
            let icon = EntryIcon::for_path(path.unwrap_or_else(|| Path::new("")), false, false);
            Entry {
                label,
                id: buffer.id,
                icon: buffer.terminal.is_none().then_some(icon),
                active: buffer.id == active,
                modified: doc.modified(),
            }
        });
        EditorHeader {
            title,
            buffers: fit_buffers(buffers, columns, spacing),
        }
    }
}

impl EditorHeader {
    /// Return the action for the screen-relative title cell (`x`, `y`), including label padding.
    pub(in crate::grid) fn target_at(&self, x: usize, y: usize) -> Option<HeaderTarget> {
        if !self.title.contains(x, y) {
            return None;
        }
        let column = x - self.title.x;
        self.buffers.iter().find_map(|item| {
            let (target, columns) = item.hit.as_ref()?;
            columns.contains(&column).then_some(*target)
        })
    }

    /// Draw the fitted labels into `frame` using `spacing`, returning the end column for the border gap.
    pub(super) fn paint(&self, frame: &mut Frame, spacing: &TextSpacing) -> Option<usize> {
        for item in &self.buffers {
            let mut columns = item.columns.clone();
            if let Some(icon) = item.icon {
                frame.icon(columns.start, 0, icon.image());
                columns.start += ICON_COLUMNS;
            }
            let color = if item.active {
                ACCENT
            } else if item.hit.is_some() {
                TEXT
            } else {
                MUTED
            };
            frame.label(0, columns, &item.label, color, spacing);
        }
        self.buffers.last().map(|item| item.columns.end)
    }
}

/// Fit a contiguous slice of `entries` into `columns`, with overflow indicators.
/// `spacing` measures header glyphs in grid cells.
fn fit_buffers(
    entries: impl Iterator<Item = Entry>,
    columns: Range<usize>,
    spacing: &TextSpacing,
) -> Vec<Segment> {
    if columns.is_empty() {
        return Vec::new();
    }
    let entries: Vec<_> = entries.collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let active = entries.iter().position(|item| item.active).unwrap_or(0);
    let widths: Vec<_> = entries.iter().map(|item| item.width(spacing)).collect();
    let separator = spacing.columns(SHORTCUT_SEPARATOR);
    let bar = GridTabBar::fit(
        entries.len(),
        |index| widths[index],
        active,
        columns,
        TabOptions {
            gap: separator,
            hit_padding: LABEL_HIT_PADDING,
            minimum: 1 + usize::from(entries[active].icon.is_some()) * ICON_COLUMNS,
            growth: TabGrowth::Narrowest,
            align: TabAlign::Start,
        },
    );
    bar.slots
        .into_iter()
        .map(|slot| {
            let columns = slot.columns;
            let (label, target, icon, active) = match slot.kind {
                TabKind::Gap => (SHORTCUT_SEPARATOR.to_owned(), None, None, false),
                TabKind::Before { count, target } => (
                    format!("{count}← "),
                    Some(HeaderTarget::Overflow(entries[target].id)),
                    None,
                    false,
                ),
                TabKind::After { count, target } => (
                    format!(" →{count}"),
                    Some(HeaderTarget::Overflow(entries[target].id)),
                    None,
                    false,
                ),
                TabKind::Tab(index) => {
                    let item = &entries[index];
                    let icon = item.icon.filter(|_| columns.len() > ICON_COLUMNS);
                    let width = columns.len() - usize::from(icon.is_some()) * ICON_COLUMNS;
                    let marker_width = spacing.columns(MODIFIED);
                    let marker = item.modified && width > marker_width;
                    let mut label = spacing
                        .ellipsize(&item.label, width - usize::from(marker) * marker_width)
                        .into_owned();
                    if marker {
                        label.push_str(MODIFIED);
                    }
                    (
                        label,
                        Some(HeaderTarget::Buffer(item.id)),
                        icon,
                        item.active,
                    )
                }
            };
            Segment {
                columns,
                label,
                hit: target.map(|target| (target, slot.hit)),
                icon,
                active,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FontId, NoopTextSystem};
    use std::sync::Arc;

    /// Build font measurements with fixed narrow-cell advances for header geometry.
    fn spacing() -> TextSpacing {
        TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.)
    }

    /// Model the named file at `id` with an icon and its `active` status.
    fn buffer(id: u64, name: &str, active: bool) -> Entry {
        Entry {
            label: name.to_owned(),
            id,
            icon: Some(EntryIcon::for_path(Path::new(name), false, false)),
            active,
            modified: false,
        }
    }

    /// Hidden counts use adjacent buffer IDs, and hit testing uses the measured display columns.
    #[test]
    fn header_hits_active_file_and_both_overflow_counters() {
        let spacing = spacing();
        let entries = [
            buffer(1, "very_long.rs", false),
            buffer(2, "wide.md", false),
            buffer(3, "active.rs", true),
            buffer(4, "next.rs", false),
        ];
        let header = EditorHeader {
            title: EditorWindow::grid_window(Rect {
                x: 5,
                y: 7,
                w: 24,
                h: 8,
            })
            .title(),
            buffers: fit_buffers(entries.into_iter(), 3..21, &spacing),
        };
        let labels: Vec<_> = header
            .buffers
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, ["2← ", "active.rs", " →1"]);
        assert_eq!(header.target_at(8, 7), Some(HeaderTarget::Overflow(2)));
        assert_eq!(header.target_at(10, 7), Some(HeaderTarget::Overflow(2)));
        assert_eq!(header.target_at(11, 7), Some(HeaderTarget::Buffer(3)));
        assert_eq!(header.target_at(22, 7), Some(HeaderTarget::Overflow(4)));
        assert_eq!(header.target_at(24, 7), Some(HeaderTarget::Overflow(4)));
        assert_eq!(header.target_at(11, 8), None);
    }

    /// Each clickable label includes the adjacent blank cell up to the separator or border.
    #[test]
    fn header_hits_padding_but_leaves_the_separator_inert() {
        let entries = [buffer(1, "a.rs", true), buffer(2, "b.rs", false)];
        let header = EditorHeader {
            title: Rect {
                x: 5,
                y: 7,
                w: 30,
                h: 1,
            },
            buffers: fit_buffers(entries.into_iter(), 3..27, &spacing()),
        };
        for x in [7, 8, 14] {
            assert_eq!(header.target_at(x, 7), Some(HeaderTarget::Buffer(1)));
        }
        for x in [16, 17, 23] {
            assert_eq!(header.target_at(x, 7), Some(HeaderTarget::Buffer(2)));
        }
        for x in [6, 15, 24] {
            assert_eq!(header.target_at(x, 7), None);
        }
    }

    /// Preserve an active dirty file's icon and marker when its full Unicode basename is too wide.
    #[test]
    fn active_buffer_ellipsizes_without_cutting_a_grapheme() {
        let spacing = spacing();
        let mut entry = buffer(1, "long-file-名字.rs", true);
        entry.modified = true;
        let items = fit_buffers(std::iter::once(entry), 3..13, &spacing);
        assert_eq!(items[0].label, "lon… [+]");
        assert!(items[0].icon.is_some());
        assert!(items[0].active);
    }

    /// The file list uses the full visible part of its editor border.
    #[test]
    fn file_list_uses_the_full_visible_editor_border() {
        let mut doc = nanobug::document::Document::new("");
        doc.path = Some(Path::new("long-current-file.rs").into());
        let editor = EditorWindow::new(doc);
        let area = Rect {
            x: 16,
            y: 4,
            w: 44,
            h: 8,
        };
        let spacing = spacing();
        let header = editor.header(area, &spacing, 40);
        assert_eq!(header.buffers.len(), 1);
        assert_eq!(header.buffers[0].columns.end, 21);
        assert_eq!(
            header.target_at(36, 4),
            Some(HeaderTarget::Buffer(editor.buffer().id))
        );
        assert_eq!(
            header.target_at(37, 4),
            Some(HeaderTarget::Buffer(editor.buffer().id))
        );
        assert_eq!(header.target_at(38, 4), None);
        let collapsed = editor.header(Rect { h: 1, ..area }, &spacing, 40);
        assert!(collapsed.buffers.is_empty());
        assert_eq!(collapsed.target_at(36, 4), None);
    }
}
