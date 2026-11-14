//! Lay out the buffer and Tabpage labels once for both painting and pointer hit testing.

use super::EditorWindow;
use crate::{
    grid::render::{Frame, OutlineStyle, Rect, SHORTCUT_SEPARATOR},
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
/// Minimum buffer space reserved when the page list grows.
const BUFFER_COLUMNS: usize = 12;
/// Grid columns between the two lists when both fill their slots.
const GROUP_GAP: usize = 3;
/// Suffix for an edited buffer.
const MODIFIED: &str = " [+]";

/// Pointer actions associated with the visible labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum HeaderTarget {
    /// A file label, identified by its stable buffer ID.
    Buffer(u64),
    /// A buffer overflow indicator that jumps to the adjacent hidden buffer.
    Overflow(u64),
    /// A zero-based project Tabpage index.
    Tabpage(usize),
}

/// One entry and its text before fitting the group to the header.
struct Entry {
    /// Name or number displayed on the border.
    label: String,
    /// Target when the user presses this label.
    target: HeaderTarget,
    /// Optional file-type artwork.
    icon: Option<EntryIcon>,
    /// Whether this entry is the current buffer or Tabpage.
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
    /// Side effect associated with pressing this segment.
    target: Option<HeaderTarget>,
    /// File icon if its required space fits.
    icon: Option<EntryIcon>,
    /// Whether this is the selected entry in its group.
    active: bool,
}

/// Editor header geometry for one grid rectangle.
pub(in crate::grid) struct EditorHeader {
    /// Screen-relative editor rectangle in grid cells.
    area: Rect,
    /// File labels and their separators in left-to-right order.
    buffers: Vec<Segment>,
    /// Tabpage labels and their separators in left-to-right order.
    pages: Vec<Segment>,
}

impl EditorWindow {
    /// Place file labels and, for multiple pages, page numbers inside `area`.
    /// Measure labels with `spacing` and clip to the `visible_cols` edge.
    pub(in crate::grid) fn header(
        &self,
        area: Rect,
        spacing: &TextSpacing,
        visible_cols: usize,
    ) -> EditorHeader {
        let visible = Rect {
            w: area.w.min(visible_cols.saturating_sub(area.x)),
            ..area
        };
        let columns = visible.border_title_columns(None);
        let page_count = self.tabpage_count();
        let active_page = self.active_tabpage();
        let pages = (0..page_count).map(|page| Entry {
            label: (page + 1).to_string(),
            target: HeaderTarget::Tabpage(page),
            icon: None,
            active: page == active_page,
            modified: false,
        });
        let page_budget = if page_count > 1 {
            let active_page_width = spacing.columns(&(active_page + 1).to_string());
            let after_gap = columns.len().saturating_sub(GROUP_GAP);
            after_gap
                .saturating_sub(BUFFER_COLUMNS)
                .max(active_page_width)
                .min(after_gap.saturating_sub(ICON_COLUMNS + 1))
        } else {
            0
        };
        let mut pages = fit_group(pages, 0..page_budget, spacing);
        let page_width = pages.last().map_or(0, |item| item.columns.end);
        let page_start = columns.end - page_width;
        for item in &mut pages {
            item.columns.start += page_start;
            item.columns.end += page_start;
        }

        let buffer_end = if pages.is_empty() {
            columns.end
        } else {
            page_start.saturating_sub(GROUP_GAP)
        };
        let active = self.buffer().id;
        let buffers = self.visible_buffers().map(|buffer| {
            let doc = &buffer.vim.doc;
            let path = doc.path.as_deref();
            let label = path
                .and_then(Path::file_name)
                .map_or_else(
                    || i18n::text("editor.untitled").into(),
                    |name| name.to_string_lossy().into_owned(),
                )
                .replace(['\r', '\n', '\t'], " ");
            let icon = EntryIcon::for_path(path.unwrap_or_else(|| Path::new("")), false, false);
            Entry {
                label,
                target: HeaderTarget::Buffer(buffer.id),
                icon: Some(icon),
                active: buffer.id == active,
                modified: doc.modified(),
            }
        });
        EditorHeader {
            area,
            buffers: fit_group(buffers, columns.start..buffer_end, spacing),
            pages,
        }
    }
}

impl EditorHeader {
    /// Return the label under the screen-relative grid cell (`x`, `y`), if one is actionable.
    pub(in crate::grid) fn target_at(&self, x: usize, y: usize) -> Option<HeaderTarget> {
        if y != self.area.y || !self.area.contains(x, y) {
            return None;
        }
        let column = x - self.area.x;
        self.buffers
            .iter()
            .chain(&self.pages)
            .find(|item| item.columns.contains(&column))
            .and_then(|item| item.target)
    }

    /// Draw the fitted text, icons and their outline gaps into `frame` using `spacing`.
    /// `selected` controls the editor outline independently of the highlighted labels.
    pub(super) fn paint(&self, frame: &mut Frame, spacing: &TextSpacing, selected: bool) {
        for item in self.buffers.iter().chain(&self.pages) {
            let mut columns = item.columns.clone();
            if let Some(icon) = item.icon {
                frame.icon(columns.start, 0, icon.image());
                columns.start += ICON_COLUMNS;
            }
            let color = if item.active {
                ACCENT
            } else if item.target.is_some() {
                TEXT
            } else {
                MUTED
            };
            frame.label(0, columns, &item.label, color, spacing);
        }
        let gaps = [&self.buffers, &self.pages]
            .into_iter()
            .filter_map(|group| {
                Some(group.first()?.columns.start.saturating_sub(1)..group.last()?.columns.end + 1)
            });
        frame.outline(
            Rect {
                x: 0,
                y: 0,
                ..self.area
            },
            if selected { ACCENT } else { MUTED },
            OutlineStyle::Solid,
            gaps,
        );
    }
}

/// Fit a contiguous slice of `entries` into `columns`, with overflow indicators.
/// `spacing` measures header glyphs in grid cells.
fn fit_group(
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
    let total = entries.len();

    let mut range = active..active + 1;
    let mut width = widths[active];
    // Grow from the active item and charge each candidate for the counters that will remain.
    loop {
        let left = range.start.checked_sub(1);
        let right = (range.end < total).then_some(range.end);
        let choices = [
            left.map(|index| (index..range.end, widths[index])),
            right.map(|index| (range.start..index + 1, widths[index])),
        ];
        let Some((next, added)) = choices
            .into_iter()
            .flatten()
            .filter(|(next, added)| {
                width + added + separator + overflow_width(next.start, total - next.end)
                    <= columns.len()
            })
            .min_by_key(|(_, added)| *added)
        else {
            break;
        };
        range = next;
        width += added + separator;
    }

    let hidden_left = range.start;
    let hidden_right = total - range.end;
    let left_width = overflow_side(hidden_left);
    let right_width = overflow_side(hidden_right);
    let active_min = 1 + usize::from(entries[active].icon.is_some()) * ICON_COLUMNS;
    let available = columns.len().saturating_sub(active_min);
    let show_left = left_width != 0 && left_width <= available;
    let show_right =
        right_width != 0 && right_width <= available - if show_left { left_width } else { 0 };
    let mut segments = Vec::new();
    let mut position = columns.start;
    if show_left {
        let target = entries[hidden_left - 1].target;
        let label = format!("{hidden_left}← ");
        position += left_width;
        segments.push(overflow_segment(columns.start..position, target, label));
    }
    let end = columns.end - if show_right { right_width } else { 0 };
    let first = range.start;
    let last = range.end;
    for index in range {
        if index != first {
            segments.push(Segment {
                columns: position..position + separator,
                label: SHORTCUT_SEPARATOR.into(),
                target: None,
                icon: None,
                active: false,
            });
            position += separator;
        }
        let item = &entries[index];
        let slot = widths[index].min(end - position);
        let icon = item.icon.filter(|_| slot > ICON_COLUMNS);
        let name_width = slot - usize::from(icon.is_some()) * ICON_COLUMNS;
        let marker = item.modified && name_width > spacing.columns(MODIFIED);
        let name_width = name_width - usize::from(marker) * spacing.columns(MODIFIED);
        let mut label = spacing.ellipsize(&item.label, name_width).into_owned();
        if marker {
            label.push_str(MODIFIED);
        }
        segments.push(Segment {
            columns: position..position + slot,
            label,
            target: Some(item.target),
            icon,
            active: item.active,
        });
        position += slot;
    }
    if show_right {
        let label = format!(" →{hidden_right}");
        segments.push(overflow_segment(
            position..position + right_width,
            entries[last].target,
            label,
        ));
    }
    segments
}

/// Count the cells for a label of hidden `count` entries, including the separating space.
fn overflow_side(count: usize) -> usize {
    if count == 0 {
        0
    } else {
        count.ilog10() as usize + 3
    }
}

/// Return the combined label width for the hidden counts on the `left` and `right`.
fn overflow_width(left: usize, right: usize) -> usize {
    overflow_side(left) + overflow_side(right)
}

/// Build an indicator from its `columns`, adjacent `target`, and formatted `label`.
fn overflow_segment(columns: Range<usize>, target: HeaderTarget, label: String) -> Segment {
    let target = match target {
        HeaderTarget::Buffer(id) => HeaderTarget::Overflow(id),
        page => page,
    };
    Segment {
        columns,
        label,
        target: Some(target),
        icon: None,
        active: false,
    }
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
            target: HeaderTarget::Buffer(id),
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
            area: Rect {
                x: 5,
                y: 7,
                w: 24,
                h: 8,
            },
            buffers: fit_group(entries.into_iter(), 3..21, &spacing),
            pages: Vec::new(),
        };
        let labels: Vec<_> = header
            .buffers
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, ["2← ", "active.rs", " →1"]);
        assert_eq!(header.target_at(8, 7), Some(HeaderTarget::Overflow(2)));
        assert_eq!(header.target_at(11, 7), Some(HeaderTarget::Buffer(3)));
        assert_eq!(header.target_at(24, 7), Some(HeaderTarget::Overflow(4)));
        assert_eq!(header.target_at(11, 8), None);
    }

    /// Preserve an active dirty file's icon and marker when its full Unicode basename is too wide.
    #[test]
    fn active_buffer_ellipsizes_without_cutting_a_grapheme() {
        let spacing = spacing();
        let mut entry = buffer(1, "long-file-名字.rs", true);
        entry.modified = true;
        let items = fit_group(std::iter::once(entry), 3..13, &spacing);
        assert_eq!(items[0].label, "lon… [+]");
        assert!(items[0].icon.is_some());
        assert!(items[0].active);
    }

    /// Clickable numeric page labels keep an inert separator between their target cells.
    #[test]
    fn tabpage_list_separates_one_based_labels() {
        let spacing = spacing();
        let pages = (0..3).map(|index| Entry {
            label: (index + 1).to_string(),
            target: HeaderTarget::Tabpage(index),
            icon: None,
            active: index == 1,
            modified: false,
        });
        let items = fit_group(pages, 8..19, &spacing);
        let text: String = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(text, "1 | 2 | 3");
        assert_eq!(items[2].columns, 12..13);
        assert_eq!(items[2].target, Some(HeaderTarget::Tabpage(1)));
        assert!(items[2].active);
        assert_eq!(items[3].target, None);
    }

    /// Page numbers appear only for multiple pages and fit within the visible grid.
    #[test]
    fn tabpage_stays_inside_the_visible_grid_at_high_zoom() {
        let mut editor = EditorWindow::new(nanobug::document::Document::new(""));
        let area = Rect {
            x: 16,
            y: 4,
            w: 44,
            h: 8,
        };
        let spacing = spacing();
        assert!(editor.header(area, &spacing, 40).pages.is_empty());
        editor.add_tabpage(0.);
        let header = editor.header(area, &spacing, 40);
        let page = &header.pages[0];
        assert_eq!(header.target_at(36, 4), Some(HeaderTarget::Tabpage(1)));
        assert_eq!(header.target_at(56, 4), None);
        assert!(header.buffers.last().unwrap().columns.end + GROUP_GAP <= page.columns.start);
        assert!(editor.remove_tabpage(0.));
        let header = editor.header(area, &spacing, 40);
        assert!(header.pages.is_empty());
        assert_eq!(header.target_at(36, 4), None);
    }
}
