use std::ops::Range;

/// Include the blank cell on either side of measured title `columns`.
pub(crate) fn title_hit_columns(columns: Range<usize>) -> Range<usize> {
    if columns.is_empty() {
        return columns;
    }
    columns.start.saturating_sub(1)..columns.end.saturating_add(1)
}
