//! Local page navigation and live filtering for the plugin browser.
use super::{Detail, Input, ManagerWindow, Page, Prompt};
use nanobug::plugins::session::Action;
impl ManagerWindow {
    /// Switch the visible tab and return any session action needed to populate it.
    pub(in crate::grid) fn select_tab(&mut self, index: u8) -> Option<Action> {
        let mut action = None;
        let manager = &mut *self;
        manager.comparison = None;
        if manager.detail.is_some() {
            manager.detail_page = match index {
                0 => Detail::Overview,
                1 => Detail::Modules,
                2 => Detail::Rules,
                3 => Detail::Logs,
                _ => return None,
            };
        } else {
            let page = match index {
                0 => Page::Installed,
                1 => Page::Marketplace,
                2 => Page::Zip,
                _ => return None,
            };
            if manager.page == page {
                return None;
            }
            manager.page = page;
            if page == Page::Marketplace {
                action = Some(Action::OpenMarketplace);
            }
        }
        self.selection = 0;
        self.offset = 0;
        action
    }
    /// Start editing the current query in the footer, keeping the prior selection for Escape.
    pub(in crate::grid) fn open_search(&mut self) {
        let manager = self;
        manager.prompt = Some(Input {
            kind: Prompt::Search {
                original: manager.search.clone(),
                selection: manager.selection,
                offset: manager.offset,
            },
            value: manager.search.clone(),
        });
    }
    /// Apply confirmed footer text and restart navigation at the first matching row.
    pub(in crate::grid) fn live_search(&mut self) {
        let manager = self;
        if let Some(input) = &manager.prompt
            && matches!(input.kind, Prompt::Search { .. })
        {
            manager.search.clone_from(&input.value);
            manager.selection = 0;
            manager.offset = 0;
        }
    }
}
