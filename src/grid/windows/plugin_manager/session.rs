//! Stable browsing state for reopening the plugin manager in a project.

use super::{Detail, Input, ManagerWindow, Page, Prompt};
use serde::{Deserialize, Serialize};

/// The selected manager pages, committed filter and list position.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(in crate::grid) struct ManagerSnapshot {
    /// Package source selected in the top-level list.
    page: Page,
    /// Package namespace selected for details.
    detail: Option<String>,
    /// Active stable detail tab.
    detail_page: Detail,
    /// Current row and list scroll, plus the row to return to from details.
    position: (usize, usize, usize),
    /// Committed search query.
    search: String,
    /// Whether the retained-data section is expanded.
    retained_open: bool,
    /// Global and project-local settings scope toggles, respectively.
    scope: (bool, bool),
}

impl ManagerWindow {
    /// Capture browsing state without capturing prompt input or pending operations.
    pub(in crate::grid) fn project_snapshot(&self) -> ManagerSnapshot {
        let (search, selection, offset) = match &self.prompt {
            Some(Input {
                kind:
                    Prompt::Search {
                        original,
                        selection,
                        offset,
                    },
                ..
            }) => (original, *selection, *offset),
            _ => (&self.search, self.selection, self.offset),
        };
        let (detail_page, selection, offset) = match self.detail_page {
            Detail::Compare => self
                .comparison
                .as_ref()
                .map_or((Detail::Rules, 0, 0), |value| value.from),
            Detail::Cleanup => (Detail::Overview, 0, 0),
            page => (page, selection, offset),
        };
        ManagerSnapshot {
            page: self.page,
            detail: self.detail.clone(),
            detail_page,
            position: (selection, offset, self.list_selection),
            search: search.clone(),
            retained_open: self.retained_open,
            scope: (self.all_projects, self.local_project),
        }
    }

    /// Apply the browsable pages and list positions from `snapshot`.
    pub(in crate::grid) fn restore_project_snapshot(&mut self, snapshot: ManagerSnapshot) {
        self.reset_list_tabs();
        self.page = snapshot.page;
        self.detail = snapshot.detail;
        self.detail_page = snapshot.detail_page;
        (self.selection, self.offset, self.list_selection) = snapshot.position;
        self.search = snapshot.search;
        self.retained_open = snapshot.retained_open;
        (self.all_projects, self.local_project) = snapshot.scope;
    }
}
