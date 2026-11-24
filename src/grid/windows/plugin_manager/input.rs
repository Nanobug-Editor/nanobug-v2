//! Local page navigation and live filtering for the plugin browser.
use super::{Detail, Input, ManagerWindow, Page, Prompt};
use nanobug::plugins::session::Action;

/// The workspace action required after closing a title in the plugin browser.
pub(in crate::grid) enum ListTabClose {
    /// Close the browser after its last page.
    Window,
    /// A surviving page is visible and may need data from the plugin session.
    Page(Option<Action>),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closing a title preserves other pages and the last visible title owns the window close.
    #[test]
    fn closes_only_the_selected_list_title() {
        let mut manager = ManagerWindow::default();
        assert!(matches!(
            manager.close_list_tab(Page::Zip),
            ListTabClose::Page(None)
        ));
        assert!(manager.page == Page::Installed);
        assert!(manager.select_tab(Page::Zip as u8).is_none());
        assert!(manager.page == Page::Installed);
        assert!(matches!(
            manager.close_list_tab(Page::Installed),
            ListTabClose::Page(Some(Action::OpenMarketplace))
        ));
        assert!(manager.page == Page::Marketplace);
        assert!(matches!(
            manager.close_list_tab(Page::Marketplace),
            ListTabClose::Window
        ));
        manager.reset_list_tabs();
        assert!(
            Page::ALL
                .into_iter()
                .all(|page| manager.list_tab_open(page))
        );
    }
}

impl ManagerWindow {
    /// Start plugin browsing with every source title visible.
    pub(in crate::grid) fn reset_list_tabs(&mut self) {
        self.closed_list_tabs = [false; Page::ALL.len()];
    }

    /// Closed pages keep their stable numeric binding and do not appear as titles.
    pub(super) fn list_tab_open(&self, page: Page) -> bool {
        !self.closed_list_tabs[page as usize]
    }

    /// Close `page` and select its next surviving neighbor when it was active.
    pub(in crate::grid) fn close_list_tab(&mut self, page: Page) -> ListTabClose {
        if !self.list_tab_open(page) {
            return ListTabClose::Page(None);
        }
        let index = page as usize;
        let neighbor = Page::ALL[index + 1..]
            .iter()
            .chain(Page::ALL[..index].iter().rev())
            .copied()
            .find(|&page| self.list_tab_open(page));
        let Some(neighbor) = neighbor else {
            return ListTabClose::Window;
        };
        self.closed_list_tabs[index] = true;
        let action = if self.page == page {
            self.select_tab(neighbor as u8)
        } else {
            None
        };
        ListTabClose::Page(action)
    }

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
            let page = *Page::ALL.get(index as usize)?;
            if manager.page == page || !manager.list_tab_open(page) {
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
        manager.prompt = Some(Input::new(
            Prompt::Search {
                original: manager.search.clone(),
                selection: manager.selection,
                offset: manager.offset,
            },
            manager.search.clone(),
        ));
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
