//! Neovim's bounded list stack, with an independent selected entry in each snapshot.

use super::model::List;
use nanobug::{i18n, vim::QuickfixCommand};

const MAX_LISTS: usize = 10;

#[derive(Clone, Default)]
pub(super) struct History {
    lists: Vec<List>,
    selected: usize,
}

impl History {
    pub(super) fn current(&self) -> Option<&List> {
        self.lists.get(self.selected)
    }

    pub(super) fn current_mut(&mut self) -> Option<&mut List> {
        self.lists.get_mut(self.selected)
    }

    /// Creating a list from an older snapshot discards its newer branch.
    pub(super) fn push(&mut self, list: List) {
        self.lists.truncate(self.selected + 1);
        if self.lists.len() == MAX_LISTS {
            self.lists.remove(0);
        }
        self.selected = self.lists.len();
        self.lists.push(list);
    }

    /// Diagnostic exports update the named Quickfix list or the current location list.
    pub(super) fn update_diagnostics(
        &mut self,
        location: bool,
        open: bool,
        mut list: List,
    ) -> bool {
        let existing = if location {
            self.current().map(|_| self.selected)
        } else {
            self.lists
                .iter()
                .position(|other| other.title == list.title)
        };
        if let Some(index) = existing {
            list.preserve_selection(&self.lists[index]);
            self.lists[index] = list;
            if open {
                self.selected = index;
            }
            self.selected == index
        } else {
            self.push(list);
            true
        }
    }

    /// Relative over-travel still selects the reachable boundary before reporting its error.
    pub(super) fn select(&mut self, command: QuickfixCommand) -> Result<(), &'static str> {
        match command {
            QuickfixCommand::Older(count) => {
                let next = self.selected.checked_sub(count.max(1));
                self.selected = next.unwrap_or(0);
                next.map(|_| ()).ok_or("quickfix.oldest")
            }
            QuickfixCommand::Newer(count) => {
                let last = self.lists.len().saturating_sub(1);
                let next = self.selected.saturating_add(count.max(1));
                self.selected = next.min(last);
                (next <= last).then_some(()).ok_or("quickfix.newest")
            }
            QuickfixCommand::History(Some(number)) if (1..=self.lists.len()).contains(&number) => {
                self.selected = number - 1;
                Ok(())
            }
            _ => Err("quickfix.invalid_history"),
        }
    }

    /// Format the currently selected list or all numbered lists for Ex output.
    pub(super) fn description(&self, all: bool) -> String {
        if self.lists.is_empty() {
            return i18n::text("quickfix.no_history").into();
        }
        self.lists
            .iter()
            .enumerate()
            .filter(|(index, _)| all || *index == self.selected)
            .map(|(index, list)| {
                let mark = if !all {
                    ""
                } else if index == self.selected {
                    "> "
                } else {
                    "  "
                };
                let line = i18n::format(
                    "quickfix.history_item",
                    &[
                        ("index", &(index + 1)),
                        ("total", &self.lists.len()),
                        ("count", &list.entries.len()),
                        ("title", &list.title),
                    ],
                );
                format!("{mark}{line}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::workspace::quickfix::Entry;
    use nanobug_plugin_sdk::DiagnosticSeverity;

    fn snapshot(title: &str, lines: &[usize]) -> List {
        List {
            title: title.into(),
            entries: lines
                .iter()
                .map(|&line| Entry {
                    buffer: 1,
                    path: None,
                    revision: 0,
                    offset: line,
                    line,
                    column: 0,
                    severity: DiagnosticSeverity::Error,
                    message: format!("line {line}"),
                    source: "test".into(),
                })
                .collect(),
            ..List::default()
        }
    }

    #[test]
    fn cap_branch_and_out_of_range_navigation_match_neovim() {
        let mut history = History::default();
        for i in 0..12 {
            history.push(snapshot(&i.to_string(), &[i]));
        }
        assert_eq!(history.lists.len(), 10);
        assert_eq!(history.lists[0].title, "2");
        assert_eq!(
            history.select(QuickfixCommand::Older(20)),
            Err("quickfix.oldest")
        );
        assert_eq!(history.current().unwrap().title, "2");
        assert_eq!(
            history.select(QuickfixCommand::Newer(20)),
            Err("quickfix.newest")
        );
        assert_eq!(history.current().unwrap().title, "11");
        history.select(QuickfixCommand::History(Some(3))).unwrap();
        assert_eq!(
            history.select(QuickfixCommand::History(Some(11))),
            Err("quickfix.invalid_history")
        );
        history.push(snapshot("filtered", &[]));
        assert_eq!(history.lists.len(), 4);
        assert_eq!(history.lists[2].title, "4");
        assert_eq!(history.current().unwrap().title, "filtered");
    }

    #[test]
    fn same_title_updates_preserve_the_entry_and_only_activate_when_open() {
        let mut history = History::default();
        history.update_diagnostics(false, false, snapshot("Diagnostics", &[3, 9]));
        history.current_mut().unwrap().current = 1;
        history.update_diagnostics(false, false, snapshot("Errors", &[100]));
        assert!(!history.update_diagnostics(false, false, snapshot("Diagnostics", &[1, 3, 9])));
        assert_eq!(history.lists.len(), 2);
        assert_eq!(history.current().unwrap().title, "Errors");
        assert_eq!(history.lists[0].current, 2);
        assert!(history.update_diagnostics(false, true, snapshot("Diagnostics", &[3, 9, 20])));
        assert_eq!(history.selected, 0);
        assert_eq!(history.current().unwrap().current, 1);
    }

    #[test]
    fn location_exports_replace_the_current_list_even_under_a_new_title() {
        let mut history = History::default();
        history.update_diagnostics(true, false, snapshot("Diagnostics", &[3, 9]));
        let filtered = history
            .current()
            .unwrap()
            .filtered("filtered".into(), |e| e.line == 9);
        history.push(filtered);
        assert_eq!(history.current().unwrap().entries.len(), 1);
        history.update_diagnostics(true, false, snapshot("Warnings", &[9]));
        assert_eq!(history.lists.len(), 2);
        assert_eq!(history.current().unwrap().title, "Warnings");
        history.select(QuickfixCommand::Older(1)).unwrap();
        assert_eq!(history.current().unwrap().title, "Diagnostics");
    }
}
