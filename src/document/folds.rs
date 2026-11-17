//! Retained line ranges and the visibility projection used by the editor.

use super::Document;
use std::collections::BTreeMap;
use std::ops::Range;

/// One manual or language-provider fold, including its visible header.
#[derive(Clone)]
struct Fold {
    /// Half-open range of document lines.
    rows: Range<usize>,
    /// Whether the range came from a language provider.
    provided: bool,
    /// An explicit open/closed override of the configured fold level.
    closed: Option<bool>,
    /// Whether a search hit temporarily shows this fold's contents.
    search_open: bool,
}

impl Fold {
    /// Whether this fold hides its body at zero-based nesting `depth` and folding `level`.
    fn closed_at(&self, depth: usize, level: usize) -> bool {
        self.rows.len() > 1 && self.closed.unwrap_or(depth >= level)
    }

    /// Whether `row` is in the hideable body below this fold's header.
    fn contains_body(&self, row: usize) -> bool {
        self.rows.start < row && row < self.rows.end
    }
}

/// Nested fold definitions and the flat hidden ranges derived from them.
#[derive(Clone)]
pub(super) struct Folds {
    /// Outer folds precede their inner folds, including equal-start ranges.
    definitions: Vec<Fold>,
    /// Disjoint hidden ranges, each excluding the visible header line.
    hidden: Vec<Range<usize>>,
    /// Whether any retained definition is temporarily open for a search hit.
    has_search_folds: bool,
    /// Number of nested levels open by default.
    level: usize,
    /// Whether closed folds currently hide their contents.
    enabled: bool,
}

impl Default for Folds {
    fn default() -> Self {
        Self {
            definitions: Vec::new(),
            hidden: Vec::new(),
            has_search_folds: false,
            level: 0,
            enabled: true,
        }
    }
}

/// Change the visibility of the fold containing the target line.
#[derive(Clone, Copy)]
pub(crate) enum FoldChange {
    /// Open the first closed level.
    Open,
    /// Close the first open level.
    Close,
    /// Open a closed fold or close an open fold.
    Toggle,
}

/// Location of the fold boundary to seek.
pub(crate) enum FoldEdge {
    /// Beginning of the next fold.
    Next,
    /// End of the previous fold.
    Previous,
    /// Beginning of a containing open fold.
    Start,
    /// End of a containing open fold.
    End,
}

/// Changes to the number of open fold levels.
pub(crate) enum FoldLevel {
    /// Adjust the current fold level by a signed amount.
    Change(isize),
    /// Close every fold.
    CloseAll,
    /// Open every fold.
    OpenAll,
}

impl Folds {
    /// Sort and project the current definitions to disjoint hidden line ranges.
    fn refresh(&mut self) {
        self.definitions
            .sort_by_key(|fold| (fold.rows.start, std::cmp::Reverse(fold.rows.end)));
        self.hidden.clear();
        self.has_search_folds = false;
        if !self.enabled {
            return;
        }
        let mut depth = Vec::new();
        for fold in &self.definitions {
            self.has_search_folds |= fold.search_open;
            while depth.last().is_some_and(|&end| end <= fold.rows.start) {
                depth.pop();
            }
            if !fold.search_open
                && fold.closed_at(depth.len(), self.level)
                && self
                    .hidden
                    .last()
                    .is_none_or(|outer| outer.end <= fold.rows.start)
            {
                self.hidden.push(fold.rows.start + 1..fold.rows.end);
            }
            depth.push(fold.rows.end);
        }
    }

    /// Return indices and current states of folds containing `row`, from outside in.
    fn containing(&self, row: usize) -> impl Iterator<Item = (usize, bool)> + '_ {
        self.definitions
            .iter()
            .enumerate()
            .filter(move |(_, fold)| fold.rows.contains(&row))
            .enumerate()
            .map(|(depth, (index, fold))| {
                (
                    index,
                    !fold.search_open && fold.closed_at(depth, self.level),
                )
            })
    }

    /// Maximum nesting depth among the retained fold definitions.
    fn depth(&self) -> usize {
        let mut active = Vec::new();
        let mut maximum = 0;
        for fold in &self.definitions {
            while active.last().is_some_and(|&end| end <= fold.rows.start) {
                active.pop();
            }
            active.push(fold.rows.end);
            maximum = maximum.max(active.len());
        }
        maximum
    }

    /// Restore the configured level, clearing all explicit open/closed overrides.
    fn reset_level(&mut self) {
        for fold in &mut self.definitions {
            fold.closed = None;
            fold.search_open = false;
        }
        self.refresh();
    }

    /// Expand a manual fold to include any existing ranges that cross its edges.
    fn complete_range(&self, mut rows: Range<usize>) -> Range<usize> {
        for fold in &self.definitions {
            let other = &fold.rows;
            if rows.start < other.end
                && other.start < rows.end
                && !(rows.start <= other.start && other.end <= rows.end)
                && !(other.start <= rows.start && rows.end <= other.end)
            {
                rows.start = rows.start.min(other.start);
                rows.end = rows.end.max(other.end);
            }
        }
        rows
    }

    /// Find the nearest sibling of the fold at `row` in the selected direction.
    /// Open containers also allow navigation among their direct children.
    fn sibling(&self, row: usize, forward: bool) -> Option<usize> {
        let mut containers = std::iter::once(0..usize::MAX).chain(
            self.containing(row)
                .take_while(|&(_, closed)| !closed || !self.enabled)
                .map(|(index, _)| self.definitions[index].rows.clone()),
        );
        let mut target = None;
        let mut start = 0;
        for container in &mut containers {
            let mut next_start = start;
            let mut next_row = container.start;
            for (index, fold) in self.definitions.iter().enumerate().skip(start) {
                let rows = &fold.rows;
                if rows.start >= container.end {
                    break;
                }
                if rows.start < next_row {
                    continue;
                }
                next_row = rows.end;
                if rows.contains(&row) {
                    next_start = index + 1;
                } else if forward && rows.start > row {
                    target = Some(rows.start);
                    break;
                } else if !forward && rows.end <= row {
                    target = Some(rows.end - 1);
                }
            }
            start = next_start;
        }
        target
    }

    /// Keep folds on unchanged rows, adjusting manual fold extent around edited lines.
    pub(super) fn shift(&mut self, first: usize, last: usize, rows: usize) {
        let delta = rows as isize - (last - first) as isize;
        self.definitions.retain_mut(|fold| {
            if fold.rows.end <= first {
                return true;
            }
            if fold.rows.start > last {
                fold.rows.start = fold.rows.start.saturating_add_signed(delta);
                fold.rows.end = fold.rows.end.saturating_add_signed(delta);
                return true;
            }
            if fold.provided {
                return false;
            }
            if fold.rows.start >= first && fold.rows.end <= last {
                return false;
            }
            if fold.rows.start > first {
                fold.rows.start = first + rows;
            }
            fold.rows.end = if fold.rows.end <= last {
                first + 1
            } else {
                fold.rows.end.saturating_add_signed(delta)
            };
            fold.rows.start < fold.rows.end
        });
        self.refresh();
    }
}

impl Document {
    /// Snapshot manual fold ranges and their retained open or closed state for a View script.
    pub fn ex_manual_fold_ranges(&self) -> Vec<(Range<usize>, bool)> {
        self.folds
            .definitions
            .iter()
            .enumerate()
            .filter_map(|(index, fold)| {
                if fold.provided {
                    return None;
                }
                let depth = self.folds.definitions[..index]
                    .iter()
                    .filter(|parent| {
                        parent.rows.start <= fold.rows.start && parent.rows.end >= fold.rows.end
                    })
                    .count();
                Some((fold.rows.clone(), fold.closed_at(depth, self.folds.level)))
            })
            .collect()
    }

    /// Close a fold covering half-open hidden `rows` and their preceding header.
    pub fn collapse_rows(&mut self, rows: Range<usize>) {
        if rows.start > 0 && rows.start < rows.end && rows.end <= self.starts.len() {
            self.add_fold(rows.start - 1..rows.end);
        }
    }

    /// Set the validated provider fold `ranges`, retaining explicit state for matching ranges.
    pub fn set_provider_folds(&mut self, mut ranges: Vec<Range<usize>>, retain_state: bool) {
        ranges.retain(|rows| rows.len() > 1 && rows.end <= self.starts.len());
        ranges.sort_by_key(|rows| (rows.start, std::cmp::Reverse(rows.end)));
        ranges.dedup();
        let mut states = BTreeMap::new();
        self.folds.definitions.retain(|fold| {
            if fold.provided && retain_state {
                states.insert(
                    (fold.rows.start, fold.rows.end),
                    (fold.closed, fold.search_open),
                );
            }
            !fold.provided
        });
        // Retained manual ranges form a hierarchy. Keep provider ranges that can
        // coexist with it, then reject crossings among the remaining provider ranges.
        let mut provider = Vec::new();
        let mut nesting = Vec::new();
        for rows in ranges {
            if self.folds.complete_range(rows.clone()) != rows {
                continue;
            }
            while nesting.last().is_some_and(|&end| end <= rows.start) {
                nesting.pop();
            }
            if nesting.last().is_some_and(|&end| end < rows.end) {
                continue;
            }
            nesting.push(rows.end);
            let (closed, search_open) = states
                .get(&(rows.start, rows.end))
                .copied()
                .unwrap_or((Some(false), false));
            provider.push(Fold {
                rows,
                provided: true,
                closed,
                search_open,
            });
        }
        self.folds.definitions.extend(provider);
        self.folds.refresh();
    }

    /// Expand or collapse one level at `row` and report if a fold was changed.
    pub fn toggle_fold(&mut self, row: usize) -> bool {
        self.change_fold(row, FoldChange::Toggle, 1, false)
    }

    /// Open one currently collapsed fold whose visible header is `row`.
    pub(crate) fn open_fold_at_header(&mut self, row: usize) -> bool {
        let Some(end) = self.folded_end_row(row) else {
            return false;
        };
        let Some((index, _)) = self.folds.containing(row).find(|&(index, closed)| {
            closed && self.folds.definitions[index].rows == (row..end + 1)
        }) else {
            return false;
        };
        self.folds.definitions[index].closed = Some(false);
        self.folds.refresh();
        true
    }

    /// Temporarily open the closed ancestors hiding the search result at `row`.
    /// Pass `None` to restore all previously opened folds.
    pub(crate) fn set_search_fold_row(&mut self, row: Option<usize>) -> bool {
        if !self.folds.enabled || row.is_none() && !self.folds.has_search_folds {
            return false;
        }
        let mut depth = 0;
        let mut changed = false;
        for fold in &mut self.folds.definitions {
            let search_open = if let Some(row) = row.filter(|&row| fold.rows.contains(&row)) {
                let open = fold.contains_body(row) && fold.closed_at(depth, self.folds.level);
                depth += 1;
                open
            } else {
                false
            };
            changed |= fold.search_open != search_open;
            fold.search_open = search_open;
        }
        if changed {
            self.folds.refresh();
        }
        changed
    }

    /// Restore temporary folds whose bodies the cursor has left by moving to `row`.
    pub fn update_search_fold_row(&mut self, row: usize) -> bool {
        if !self.folds.has_search_folds {
            return false;
        }
        let mut changed = false;
        for fold in &mut self.folds.definitions {
            if fold.search_open && !fold.contains_body(row) {
                fold.search_open = false;
                changed = true;
            }
        }
        if changed {
            self.folds.refresh();
        }
        changed
    }

    /// Return the currently hidden document rows enclosing `row`, excluding their visible header.
    pub fn collapsed_body_at(&self, row: usize) -> Option<Range<usize>> {
        let hidden = &self.folds.hidden;
        let index = hidden.partition_point(|range| range.end <= row);
        hidden
            .get(index)
            .filter(|range| range.contains(&row))
            .cloned()
    }

    /// Open any collapsed folds hiding document `row`.
    pub fn reveal_row(&mut self, row: usize) -> bool {
        if self.collapsed_body_at(row).is_some() {
            return self.open_folds_at(row);
        }
        false
    }

    /// Open enclosing folds on `row`, including a fold starting on that row.
    pub(crate) fn open_folds_at(&mut self, row: usize) -> bool {
        self.change_fold(row, FoldChange::Open, usize::MAX, false)
    }

    /// Convert document `row` to a display row, mapping hidden lines to their fold header.
    pub fn display_row(&self, row: usize) -> usize {
        let mut hidden = 0;
        for range in &self.folds.hidden {
            if row < range.start {
                break;
            }
            if row < range.end {
                return range.start - 1 - hidden;
            }
            hidden += range.len();
        }
        row - hidden
    }

    /// Convert a display `row` back to its visible document row.
    pub fn document_row(&self, mut row: usize) -> usize {
        for range in &self.folds.hidden {
            if row < range.start {
                break;
            }
            row += range.len();
        }
        row
    }

    /// Number of visible document rows after folds have been applied.
    pub fn display_rows(&self) -> usize {
        self.starts.len() - self.folds.hidden.iter().map(Range::len).sum::<usize>()
    }

    /// Disjoint physical row ranges hidden by the current fold projection.
    pub fn hidden_rows(&self) -> &[Range<usize>] {
        &self.folds.hidden
    }

    /// Whether a collapsed fold starts just below document `row`.
    pub fn folded_at(&self, row: usize) -> bool {
        self.folded_end_row(row).is_some()
    }

    /// Return the last hidden document row of the collapsed fold headed by `row`.
    pub fn folded_end_row(&self, row: usize) -> Option<usize> {
        self.folds
            .hidden
            .binary_search_by_key(&row, |range| range.start - 1)
            .ok()
            .map(|index| self.folds.hidden[index].end - 1)
    }

    /// Adjust fold coordinates for a byte `range` replaced with `text`.
    pub(super) fn shift_folds(&mut self, range: &Range<usize>, text: &str) {
        if self.folds.definitions.is_empty() {
            return;
        }
        let first = self.starts.partition_point(|&start| start <= range.start) - 1;
        let last = self.starts.partition_point(|&start| start <= range.end) - 1;
        let rows = text.bytes().filter(|&byte| byte == b'\n').count();
        self.folds.shift(first, last, rows);
    }

    /// Realign current manual folds when undo/redo restores `text`.
    pub(super) fn restore_folds(&mut self, text: &str) {
        if self.folds.definitions.is_empty() {
            return;
        }
        self.folds.definitions.retain(|fold| !fold.provided);
        let start = self
            .text
            .bytes()
            .zip(text.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let start = self.text.floor_char_boundary(start);
        let tail = self.text.as_bytes()[start..]
            .iter()
            .rev()
            .zip(text.as_bytes()[start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let before_end = self.text.ceil_char_boundary(self.text.len() - tail);
        let after_end = text.ceil_char_boundary(text.len() - tail);
        self.shift_folds(&(start..before_end), &text[start..after_end]);
        self.folds.refresh();
    }

    /// Create a closed manual fold over `rows`, including its header.
    pub(crate) fn add_fold(&mut self, rows: Range<usize>) {
        if rows.is_empty() || rows.end > self.starts.len() {
            return;
        }
        self.folds.definitions.push(Fold {
            rows: self.folds.complete_range(rows),
            provided: false,
            closed: Some(true),
            search_open: false,
        });
        self.folds.enabled = true;
        self.folds.refresh();
    }

    /// Change folds enclosing `row`. `count` limits the affected levels and `recursive`
    /// includes nested folds anywhere inside the affected fold.
    fn change_fold(
        &mut self,
        row: usize,
        change: FoldChange,
        count: usize,
        recursive: bool,
    ) -> bool {
        let enabled = self.folds.enabled;
        let containing = self.folds.containing(row).collect::<Vec<_>>();
        let close = match change {
            FoldChange::Open => false,
            FoldChange::Close => true,
            FoldChange::Toggle => !enabled || !containing.iter().any(|(_, closed)| *closed),
        };
        // An already-closed outer level blocks further single-level closing.
        if close && enabled && containing.first().is_some_and(|&(_, closed)| closed) {
            return false;
        }
        let mut selected = containing
            .into_iter()
            .filter(|&(_, closed)| if close { !enabled || !closed } else { closed });
        let Some((first, _)) = selected.next() else {
            return false;
        };
        if recursive {
            let outer = self.folds.definitions[first].rows.clone();
            for fold in &mut self.folds.definitions[first..] {
                if fold.rows.start >= outer.end {
                    break;
                }
                if fold.rows.end <= outer.end {
                    fold.closed = Some(close);
                    fold.search_open = false;
                }
            }
        } else {
            for index in std::iter::once(first).chain(
                selected
                    .take(count.saturating_sub(1))
                    .map(|(index, _)| index),
            ) {
                let fold = &mut self.folds.definitions[index];
                fold.closed = Some(close);
                fold.search_open = false;
            }
        }
        self.folds.enabled |= close;
        self.folds.refresh();
        true
    }

    /// Change each distinct fold touched by `rows` up to its requested `count` or recursively.
    pub(crate) fn change_fold_range(
        &mut self,
        rows: Range<usize>,
        change: FoldChange,
        count: usize,
        recursive: bool,
    ) -> bool {
        let mut changed = false;
        for row in self.touched_folds(rows) {
            changed |= self.change_fold(row, change, count, recursive);
        }
        changed
    }

    /// Apply an Ex fold command over addressed rows, stepping past each fold changed once.
    pub(crate) fn change_ex_fold_rows(
        &mut self,
        rows: Range<usize>,
        close: bool,
        recursive: bool,
    ) -> bool {
        let mut row = rows.start;
        let mut found = false;
        while row < rows.end {
            let containing: Vec<_> = self.folds.containing(row).collect();
            if containing.is_empty() {
                let next = self
                    .folds
                    .definitions
                    .partition_point(|fold| fold.rows.start <= row);
                let Some(fold) = self.folds.definitions.get(next) else {
                    break;
                };
                row = fold.rows.start;
                continue;
            }
            found = true;
            let last_open = containing.iter().take_while(|(_, closed)| !closed).last();
            let first_closed = containing.iter().find(|(_, closed)| *closed);
            let next_row = if recursive {
                row + 1
            } else if close {
                last_open.or(first_closed).map_or(row + 1, |&(index, _)| {
                    self.folds.definitions[index].rows.end
                })
            } else {
                first_closed.map_or(row + 1, |&(index, _)| {
                    self.folds.definitions[index].rows.end
                })
            };
            if close && recursive {
                for (index, _) in containing {
                    let fold = &mut self.folds.definitions[index];
                    fold.closed = Some(true);
                    fold.search_open = false;
                }
            } else if close {
                if let Some(&(index, _)) = last_open {
                    let fold = &mut self.folds.definitions[index];
                    fold.closed = Some(true);
                    fold.search_open = false;
                }
            } else if let Some(&(index, _)) = first_closed {
                let end = self.folds.definitions[index].rows.end;
                for fold in &mut self.folds.definitions[index..] {
                    if fold.rows.start >= end {
                        break;
                    }
                    fold.closed = Some(false);
                    fold.search_open = false;
                    if !recursive {
                        break;
                    }
                }
            }
            row = next_row;
        }
        if found {
            self.folds.enabled |= close;
            self.folds.refresh();
        }
        found
    }

    /// Pick one row from each outer fold intersecting the selection `rows`.
    fn touched_folds(&self, rows: Range<usize>) -> Vec<usize> {
        let mut after = 0;
        self.folds
            .definitions
            .iter()
            .filter_map(|fold| {
                if fold.rows.start < after {
                    return None;
                }
                after = fold.rows.end;
                (fold.rows.start < rows.end && rows.start < fold.rows.end)
                    .then_some(rows.start.max(fold.rows.start))
            })
            .collect()
    }

    /// Delete one enclosing manual fold at `row`, recursively when requested.
    fn delete_fold(&mut self, row: usize, recursive: bool) -> bool {
        let selected = self
            .folds
            .containing(row)
            .find_map(|(index, _)| (!self.folds.definitions[index].provided).then_some(index));
        let Some(index) = selected else { return false };
        let removed = self.folds.definitions.remove(index);
        if recursive {
            let mut at = 0;
            self.folds.definitions.retain(|fold| {
                let keep = at < index
                    || fold.provided
                    || fold.rows.start >= removed.rows.end
                    || fold.rows.end > removed.rows.end;
                at += 1;
                keep
            });
        }
        self.folds.refresh();
        true
    }

    /// Delete manual folds touched by `rows`, with nested folds included when `recursive`.
    pub(crate) fn delete_fold_range(&mut self, rows: Range<usize>, recursive: bool) -> bool {
        let mut changed = false;
        for row in self.touched_folds(rows) {
            changed |= self.delete_fold(row, recursive);
        }
        changed
    }

    /// Delete all manual folds in this buffer.
    pub(crate) fn delete_folds(&mut self) {
        self.folds.definitions.retain(|fold| fold.provided);
        self.folds.refresh();
    }

    /// Set fold visibility to `enabled`, or toggle it when unspecified.
    pub(crate) fn set_folding(&mut self, enabled: Option<bool>) {
        self.folds.enabled = enabled.unwrap_or(!self.folds.enabled);
        if !self.folds.enabled {
            for fold in &mut self.folds.definitions {
                fold.search_open = false;
            }
        }
        self.folds.refresh();
    }

    /// Update the configured fold `level` and apply it to the retained definitions.
    pub(crate) fn change_fold_level(&mut self, level: FoldLevel) {
        self.folds.level = match level {
            FoldLevel::Change(amount) => self.folds.level.saturating_add_signed(amount),
            FoldLevel::CloseAll => 0,
            FoldLevel::OpenAll => self.folds.depth(),
        };
        self.folds.enabled |= matches!(level, FoldLevel::CloseAll | FoldLevel::Change(..=-1));
        self.folds.reset_level();
    }

    /// Recompute visibility at the configured fold level.
    pub(crate) fn reset_folds(&mut self) {
        self.folds.enabled = true;
        self.folds.reset_level();
    }

    /// Find the `count`th fold boundary in the direction of `edge` from `row`.
    pub(crate) fn fold_edge(&self, mut row: usize, edge: FoldEdge, count: usize) -> Option<usize> {
        let original = row;
        for _ in 0..count {
            let target = match edge {
                FoldEdge::Next => self.folds.sibling(row, true),
                FoldEdge::Previous => self.folds.sibling(row, false),
                FoldEdge::Start | FoldEdge::End => self
                    .folds
                    .containing(row)
                    .take_while(|&(_, closed)| !closed || !self.folds.enabled)
                    .map(|(index, _)| &self.folds.definitions[index].rows)
                    .map(|fold| {
                        if matches!(edge, FoldEdge::Start) {
                            fold.start
                        } else {
                            fold.end - 1
                        }
                    })
                    .filter(|&edge| edge != row)
                    .last(),
            };
            let Some(target) = target else { break };
            row = target;
        }
        (row != original).then_some(row)
    }
}

#[cfg(test)]
mod tests {
    use super::Document;

    /// Search reveals only the containing folds, then restores them as the cursor leaves.
    #[test]
    fn search_visibility_preserves_other_folds() {
        let mut doc = Document::new(&"line\n".repeat(15));
        for rows in [0..10, 2..5, 6..9, 11..14] {
            doc.add_fold(rows);
        }
        assert_eq!(doc.collapsed_body_at(3), Some(1..10));

        assert!(doc.set_search_fold_row(Some(3)));
        assert_eq!(doc.collapsed_body_at(3), None);
        assert_eq!(doc.collapsed_body_at(7), Some(7..9));
        assert_eq!(doc.collapsed_body_at(12), Some(12..14));
        assert_eq!(doc.document_row(doc.display_row(3)), 3);
        assert!(!doc.update_search_fold_row(4));

        assert!(doc.update_search_fold_row(5));
        assert_eq!(doc.folded_end_row(2), Some(4));
        assert_eq!(doc.folded_end_row(0), None);

        assert!(doc.update_search_fold_row(0));
        assert_eq!(doc.folded_end_row(0), Some(9));
        assert_eq!(doc.folded_end_row(2), None);
    }

    /// Explicitly opening a nested header leaves a temporarily open ancestor eligible to refold.
    #[test]
    fn opening_one_header_inside_a_search_preview_preserves_its_ancestors() {
        let mut doc = Document::new(&"line\n".repeat(10));
        doc.add_fold(0..9);
        doc.add_fold(2..6);
        doc.add_fold(2..4);

        assert!(doc.set_search_fold_row(Some(2)));
        assert_eq!(doc.folded_end_row(2), Some(5));
        assert!(doc.open_fold_at_header(2));
        assert_eq!(doc.folded_end_row(2), Some(3));
        assert!(doc.update_search_fold_row(9));
        assert_eq!(doc.folded_end_row(0), Some(8));
        assert!(doc.open_fold_at_header(0));
        assert_eq!(doc.folded_end_row(2), Some(3));
    }
}
