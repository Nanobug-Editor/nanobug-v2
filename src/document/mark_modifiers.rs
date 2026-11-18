use super::Document;
use std::{cell::Cell, ops::Range};

/// Mark and history rules for one Ex command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkModifiers {
    /// Keep original coordinates for surviving lines in a filter range.
    pub keep_marks: bool,
    /// Keep original coordinates for marks and existing history entries.
    pub lock_marks: bool,
    /// Suppress recording previous context, insertion and change positions.
    pub keep_jumps: bool,
}

thread_local! {
    static MODIFIERS: Cell<MarkModifiers> = const { Cell::new(MarkModifiers {
        keep_marks: false,
        lock_marks: false,
        keep_jumps: false,
    }) };
}

/// Apply the command's rules during its synchronous work and restore the caller's rules on exit.
pub fn with_mark_modifiers<T>(modifiers: MarkModifiers, run: impl FnOnce() -> T) -> T {
    struct Restore(MarkModifiers);

    impl Drop for Restore {
        fn drop(&mut self) {
            MODIFIERS.set(self.0);
        }
    }

    let _restore = Restore(MODIFIERS.replace(modifiers));
    run()
}

/// Read the current command's rules, for use when scheduling work that completes later.
pub fn current_mark_modifiers() -> MarkModifiers {
    MODIFIERS.get()
}

/// Coordinates of retained marks before a replacement changes the line index.
pub(crate) struct MarkEdit {
    range: Range<usize>,
    inserted: usize,
    locked: bool,
    filter_rows: Option<(Range<usize>, usize)>,
    old_starts: Option<Vec<usize>>,
}

impl MarkEdit {
    /// Capture the original line index only when this edit preserves line coordinates.
    pub(crate) fn new(
        doc: &Document,
        range: Range<usize>,
        inserted: usize,
        filter_lines: Option<usize>,
    ) -> Self {
        let modifiers = current_mark_modifiers();
        let filter_rows = filter_lines
            .filter(|_| modifiers.keep_marks && !range.is_empty())
            .map(|lines| {
                let first = doc.starts.partition_point(|&start| start <= range.start) - 1;
                let last = doc.starts.partition_point(|&start| start < range.end);
                (first..last, lines)
            });
        Self {
            range,
            inserted,
            locked: modifiers.lock_marks,
            old_starts: (modifiers.lock_marks || filter_rows.is_some()).then(|| doc.starts.clone()),
            filter_rows,
        }
    }

    /// Move a mark, allowing buffer-local marks on removed filter lines to be cleared.
    pub(crate) fn position(
        &self,
        doc: &Document,
        offset: usize,
        delete_on_removed_line: bool,
    ) -> Option<usize> {
        if let Some(starts) = &self.old_starts {
            let row = starts.partition_point(|&start| start <= offset) - 1;
            let column = offset - starts[row];
            if self.locked {
                return Some(at_location(doc, row, column));
            }
            if let Some((rows, lines)) = &self.filter_rows
                && rows.contains(&row)
            {
                return if row - rows.start < *lines {
                    Some(at_location(doc, row, column))
                } else if delete_on_removed_line {
                    None
                } else {
                    Some(at_location(doc, rows.start, column))
                };
            }
        }
        Some(self.context_position(doc, offset))
    }

    /// Rebase a previous-context mark even when other mark coordinates are locked.
    pub(crate) fn context_position(&self, doc: &Document, offset: usize) -> usize {
        let offset = if offset >= self.range.end {
            offset - self.range.len() + self.inserted
        } else if offset >= self.range.start {
            self.range.start
        } else {
            offset
        };
        doc.text.floor_char_boundary(offset.min(doc.text.len()))
    }

    /// Rebase a persistent mark or list entry, placing it at the start of the edit if removed.
    pub(crate) fn retained_position(&self, doc: &Document, offset: usize) -> usize {
        self.position(doc, offset, false)
            .expect("persistent marks are retained")
    }
}

/// Resolve a logical mark to an offset on a valid UTF-8 boundary after the edit.
fn at_location(doc: &Document, row: usize, column: usize) -> usize {
    let line = doc.line_range(row.min(doc.starts.len() - 1));
    doc.text
        .floor_char_boundary(line.start + column.min(line.len()))
}
