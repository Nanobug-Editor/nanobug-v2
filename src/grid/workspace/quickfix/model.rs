//! Retained Quickfix snapshots and their navigation indices.

use super::history::History;
use crate::grid::{windows::editor::ResultList, workspace::editor_splits::PaneId};
use nanobug::{document::DocumentAnchor, vim::QuickfixCommand};
use nanobug_plugin_sdk::DiagnosticSeverity;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// A validated diagnostic with its displayed location and its exact source snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::grid::workspace) struct Entry {
    pub(in crate::grid::workspace) buffer: u64,
    pub(in crate::grid::workspace) path: Option<PathBuf>,
    pub(in crate::grid::workspace) revision: u64,
    pub(in crate::grid::workspace) offset: usize,
    /// Zero-based line and UTF-8 byte column, as used by Quickfix.
    pub(in crate::grid::workspace) line: usize,
    pub(in crate::grid::workspace) column: usize,
    pub(in crate::grid::workspace) severity: DiagnosticSeverity,
    pub(in crate::grid::workspace) message: String,
    pub(in crate::grid::workspace) source: String,
}

impl Entry {
    /// Format a single physical row, retaining source paths relative to the project.
    pub(super) fn label(&self, root: &Path) -> String {
        let path = self
            .path
            .as_deref()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| format!("#{}", self.buffer));
        let severity = match self.severity {
            DiagnosticSeverity::Error => "E",
            DiagnosticSeverity::Warning => "W",
            DiagnosticSeverity::Information => "I",
            DiagnosticSeverity::Hint => "H",
        };
        let row = format!(
            "{path}|{}:{} {severity}| {} [{}]",
            self.line + 1,
            self.column + 1,
            self.message,
            self.source
        );
        row.replace(['\r', '\n', '\t'], " ")
    }
}

/// Current Quickfix list, independent location lists and the displayed diagnostic views.
#[derive(Default)]
pub(in crate::grid::workspace) struct Lists(BTreeMap<ResultList, History>);

/// Original entries and optional edit-tracking anchors for retained Quickfix snapshots.
#[derive(Clone, Default)]
pub(super) struct List {
    pub(super) title: String,
    pub(super) entries: Vec<Entry>,
    pub(super) anchors: Vec<Option<DocumentAnchor>>,
    pub(super) current: usize,
}

impl Lists {
    pub(super) fn current(&self, kind: ResultList) -> Option<&List> {
        self.0.get(&kind)?.current()
    }

    pub(super) fn history(&mut self, kind: ResultList) -> &mut History {
        self.0.entry(kind).or_default()
    }

    /// A newly split source window starts with an independent copy of its parent's location list.
    pub(in crate::grid::workspace) fn inherit(&mut self, parent: PaneId, child: PaneId) {
        if let Some(list) = self.0.get(&ResultList::Location(parent)).cloned() {
            self.0.insert(ResultList::Location(child), list);
        }
    }

    /// Release window-local results when the owning source window closes.
    pub(in crate::grid::workspace) fn forget(&mut self, pane: PaneId) {
        self.0.remove(&ResultList::Location(pane));
    }
}

impl List {
    /// Keep the previously selected file position when a diagnostic export refreshes its entries.
    pub(super) fn preserve_selection(&mut self, old: &Self) {
        let Some(before) = old.entries.get(old.current) else {
            return;
        };
        let position = old
            .anchors
            .get(old.current)
            .and_then(Option::as_ref)
            .map(DocumentAnchor::position);
        let (line, column) = position
            .as_ref()
            .map(|pos| pos.location)
            .unwrap_or((before.line, before.column));
        self.current = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.buffer == before.buffer)
            .min_by_key(|(_, entry)| {
                (
                    entry.line.abs_diff(line),
                    entry.column.abs_diff(column),
                    entry.message != before.message,
                )
            })
            .map_or(0, |(index, _)| index);
    }

    /// Retain matching entries alongside the original edit-tracking anchors.
    pub(super) fn filtered(&self, title: String, keep: impl Fn(&Entry) -> bool) -> Self {
        let mut list = Self {
            title,
            ..Self::default()
        };
        for (index, entry) in self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| keep(entry))
        {
            list.entries.push(entry.clone());
            list.anchors
                .push(self.anchors.get(index).cloned().flatten());
        }
        list
    }

    pub(super) fn rows(&self, root: &Path) -> String {
        self.entries
            .iter()
            .map(|entry| entry.label(root))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve Neovim's one-based navigation counts. A relative count stops at the edge.
    pub(super) fn destination(&self, command: QuickfixCommand) -> Result<usize, &'static str> {
        let last = self.entries.len().checked_sub(1).ok_or("quickfix.empty")?;
        let index = match command {
            QuickfixCommand::First => 0,
            QuickfixCommand::Last => last,
            QuickfixCommand::Current(Some(index)) => index.saturating_sub(1).min(last),
            QuickfixCommand::Current(None) => self.current,
            QuickfixCommand::Next(count) if self.current < last => {
                self.current.saturating_add(count.max(1)).min(last)
            }
            QuickfixCommand::Previous(count) if self.current > 0 => {
                self.current.saturating_sub(count.max(1))
            }
            QuickfixCommand::File { forward, count } => {
                let mut index = self.current;
                let mut found = 0;
                let mut next = index;
                while found < count.max(1) {
                    let Some(candidate) = (if forward {
                        next.checked_add(1).filter(|i| *i <= last)
                    } else {
                        next.checked_sub(1)
                    }) else {
                        break;
                    };
                    next = candidate;
                    if self.entries[index].buffer != self.entries[next].buffer {
                        index = next;
                        found += 1;
                    }
                }
                if found == 0 {
                    return Err("quickfix.boundary");
                }
                index
            }
            _ => return Err("quickfix.boundary"),
        };
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> List {
        let entries = [1, 1, 2, 2, 3, 3]
            .into_iter()
            .enumerate()
            .map(|(line, buffer)| Entry {
                buffer,
                path: None,
                revision: 0,
                offset: line,
                line,
                column: 0,
                severity: DiagnosticSeverity::Warning,
                message: "warning".into(),
                source: "plugin".into(),
            })
            .collect();
        List {
            entries,
            ..List::default()
        }
    }

    #[test]
    fn relative_counts_stop_at_the_boundary_without_wrapping() {
        let mut list = fixture();
        assert_eq!(list.destination(QuickfixCommand::Next(100)), Ok(5));
        assert_eq!(
            list.destination(QuickfixCommand::Previous(1)),
            Err("quickfix.boundary")
        );
        list.current = 5;
        assert_eq!(list.destination(QuickfixCommand::Previous(100)), Ok(0));
        assert_eq!(
            list.destination(QuickfixCommand::Next(1)),
            Err("quickfix.boundary")
        );
        assert_eq!(list.destination(QuickfixCommand::Current(Some(10))), Ok(5));
    }

    #[test]
    fn file_navigation_reaches_the_nearest_entry_in_each_distinct_file() {
        let mut list = fixture();
        list.current = 3;
        let grouped = |forward, count| QuickfixCommand::File { forward, count };
        assert_eq!(list.destination(grouped(true, 1)), Ok(4));
        assert_eq!(list.destination(grouped(false, 1)), Ok(1));
        assert_eq!(list.destination(grouped(true, 100)), Ok(4));
        list.current = 0;
        assert_eq!(list.destination(grouped(true, 2)), Ok(4));
        assert_eq!(
            list.destination(grouped(false, 1)),
            Err("quickfix.boundary")
        );
    }

    #[test]
    fn source_windows_copy_then_independently_own_their_location_lists() {
        let mut lists = Lists::default();
        lists.history(ResultList::Quickfix).push(fixture());
        lists.history(ResultList::Location(1)).push(fixture());
        lists.inherit(1, 2);
        lists
            .history(ResultList::Location(2))
            .current_mut()
            .unwrap()
            .current = 2;
        assert_eq!(lists.current(ResultList::Location(1)).unwrap().current, 0);
        assert_eq!(lists.current(ResultList::Quickfix).unwrap().current, 0);
        lists.forget(1);
        assert_eq!(lists.current(ResultList::Location(2)).unwrap().current, 2);
        assert!(lists.current(ResultList::Location(1)).is_none());
    }
}
