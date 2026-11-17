//! Shortcut scopes, filter results and the logical selection in the popup.

use super::{
    super::{Shortcut, editor, manager},
    HelpCommand, shortcuts,
};
use nanobug::{
    animation::scroll::{GridScroll, ScrollInput},
    i18n,
};
use unicode_segmentation::UnicodeSegmentation;

/// Catalog currently displayed in the popup.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::grid) enum Scope {
    /// Bindings of the pane that opened help.
    Window,
    /// Shared workspace bindings.
    Global,
}

/// A browsing session over the opening pane's shortcuts and the global shortcuts.
pub(in crate::grid) struct HelpList {
    /// Shortcut rows for the window that opened help.
    bindings: &'static [Shortcut],
    /// Translation key of that window's help title.
    title: &'static str,
    /// Whether the selected tab offers Explorer actions in the global catalog.
    manager_available: bool,
    /// Current source of shortcut rows.
    scope: Scope,
    /// The dismissed title, if the session is down to one catalog.
    closed_scope: Option<Scope>,
    /// Localized key labels and descriptions in filtered catalog order.
    pub(super) rows: Vec<(&'static str, &'static str)>,
    /// Selected index in the filtered rows, or zero when there are no matches.
    pub(in crate::grid) selected: usize,
    /// Vertical motion in list rows.
    pub(in crate::grid) scroll: GridScroll,
    /// Committed filter text shared by both scopes.
    pub(super) query: String,
    /// Query to restore on Escape; present only while editing the filter.
    previous_query: Option<String>,
    /// Decimal prefix for the next row movement.
    pub(super) count: usize,
    /// Count before the first `g` in a pending `gg` motion.
    pub(super) pending_g: Option<usize>,
}

impl HelpList {
    /// Build the unfiltered window catalog for the editor or, with `manager`, Explorer.
    pub(super) fn new(manager: bool, manager_available: bool) -> Self {
        if manager {
            Self::for_window(
                manager::shortcuts::SHORTCUTS,
                "help.explorer_title",
                manager_available,
            )
        } else {
            Self::for_window(
                editor::shortcuts::SHORTCUTS,
                "help.editor_title",
                manager_available,
            )
        }
    }

    /// Build an unfiltered session from `bindings` and the window's `title` key.
    pub(super) fn for_window(
        bindings: &'static [Shortcut],
        title: &'static str,
        manager_available: bool,
    ) -> Self {
        let mut list = Self {
            bindings,
            title,
            manager_available,
            scope: Scope::Window,
            closed_scope: None,
            rows: Vec::new(),
            selected: 0,
            scroll: GridScroll::default(),
            query: String::new(),
            previous_query: None,
            count: 0,
            pending_g: None,
        };
        list.refilter(0.);
        list
    }

    /// Original window bindings, independent of the active scope and query.
    fn window_rows(&self) -> &'static [Shortcut] {
        self.bindings
    }

    /// Largest catalog size, so filtering or switching scopes keeps the popup in place.
    pub(super) fn capacity(&self) -> usize {
        self.window_rows().len().max(shortcuts::GLOBAL.len())
    }

    /// Border title for the pane that opened help.
    pub(super) fn window_title(&self) -> &'static str {
        i18n::text(self.title)
    }

    /// Catalog currently selected in the title and displayed in the list.
    pub(super) fn scope(&self) -> Scope {
        self.scope
    }

    /// Whether `scope` still has a title in this browsing session.
    pub(super) fn has_scope(&self, scope: Scope) -> bool {
        self.closed_scope != Some(scope)
    }

    /// Remove `scope` at `now` and switch to the other catalog, or close after the last title.
    pub(super) fn close_scope(&mut self, scope: Scope, now: f64) -> HelpCommand {
        if !self.has_scope(scope) {
            return HelpCommand::Ignore;
        }
        if self.closed_scope.is_some() {
            return HelpCommand::Close;
        }

        self.closed_scope = Some(scope);
        let remaining = match scope {
            Scope::Window => Scope::Global,
            Scope::Global => Scope::Window,
        };
        self.select_scope(remaining, now);
        HelpCommand::Update(ScrollInput::Advance)
    }

    /// Number of rows in the filtered list.
    pub(in crate::grid) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Select an open `scope`, preserving the filter and rebuilding rows at `now` if it changed.
    pub(in crate::grid) fn select_scope(&mut self, scope: Scope, now: f64) -> bool {
        self.cancel_navigation();
        if self.scope == scope || !self.has_scope(scope) {
            return false;
        }
        self.scope = scope;
        self.refilter(now);
        true
    }

    /// Move the caret by signed `rows`, returning the reveal intent.
    pub(super) fn move_selection(&mut self, rows: isize) -> ScrollInput {
        self.select_row(self.selected.saturating_add_signed(rows))
    }

    /// Select zero-based `row`, clamped to the filtered list, and request a reveal.
    pub(super) fn select_row(&mut self, row: usize) -> ScrollInput {
        self.selected = row.min(self.len().saturating_sub(1));
        ScrollInput::Reveal
    }

    /// Visit the next match, or the previous match when `backward` is true, wrapping at the ends.
    pub(super) fn repeat_search(&mut self, backward: bool) -> ScrollInput {
        if self.query.trim().is_empty() || self.len() == 0 {
            return ScrollInput::Advance;
        }
        let row = if backward {
            self.selected.checked_sub(1).unwrap_or(self.len() - 1)
        } else {
            (self.selected + 1) % self.len()
        };
        self.select_row(row)
    }

    /// Discard a partial Normal motion when a different input takes over.
    pub(super) fn cancel_navigation(&mut self) {
        self.count = 0;
        self.pending_g = None;
    }

    /// Apply `input` over `height` visible rows at time `now`.
    /// Return whether retained cells changed and the animated displacement in rows.
    pub(in crate::grid) fn track(
        &mut self,
        input: ScrollInput,
        height: usize,
        now: f64,
    ) -> (bool, f32) {
        let before = (self.selected, self.scroll.first_cell());
        // The prompt owns the caret, so a selection covered by the prompt cannot pull the list up.
        let caret = (!self.is_filtering()).then_some(before.0);
        let (caret, delta) = self
            .scroll
            .track_rows(input, caret, height, self.rows.len(), now);
        self.selected = caret.unwrap_or(before.0);
        (before != (self.selected, self.scroll.first_cell()), delta)
    }

    /// Whether native text belongs to the filter prompt.
    pub(in crate::grid) fn is_filtering(&self) -> bool {
        self.previous_query.is_some()
    }

    /// Edit the current filter, retaining it so Escape can cancel the edit.
    pub(super) fn start_filter(&mut self) {
        self.previous_query = Some(self.query.clone());
    }

    /// End editing, keeping the query or restoring it on `cancel` at animation time `now`.
    pub(in crate::grid) fn finish_filter(&mut self, cancel: bool, now: f64) {
        if let Some(previous) = self.previous_query.take()
            && cancel
            && previous != self.query
        {
            self.query = previous;
            self.refilter(now);
        }
    }

    /// Append committed native `text` to the single-line filter at animation time `now`.
    pub(in crate::grid) fn commit_filter(&mut self, text: &str, now: f64) {
        let end = self.query.len();
        self.query
            .extend(text.chars().filter(|ch| !ch.is_control()));
        if self.query.len() != end {
            self.refilter(now);
        }
    }

    /// Remove the last grapheme and refresh matches at animation time `now`.
    pub(super) fn filter_backspace(&mut self, now: f64) {
        if let Some((byte, _)) = self.query.grapheme_indices(true).next_back() {
            self.query.truncate(byte);
            self.refilter(now);
        }
    }

    /// Rebuild matches against localized descriptions and reset list position at `now`.
    fn refilter(&mut self, now: f64) {
        let source = match self.scope {
            Scope::Window => self.window_rows(),
            Scope::Global => shortcuts::GLOBAL,
        };
        let query = self.query.trim().to_lowercase();
        self.rows.clear();
        self.rows
            .extend(source.iter().filter_map(|(keys, description)| {
                if self.scope == Scope::Global
                    && !self.manager_available
                    && matches!(
                        *description,
                        "global_shortcuts.toggle_manager"
                            | "global_shortcuts.focus_horizontal"
                            | "global_shortcuts.refresh"
                    )
                {
                    return None;
                }
                let description = i18n::text(description);
                (query.is_empty() || description.to_lowercase().contains(&query))
                    .then(|| (keys.text(), description))
            }));
        self.selected = 0;
        self.scroll.snap(0, now);
    }
}
