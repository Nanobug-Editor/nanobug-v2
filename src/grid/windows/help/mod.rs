//! Shortcut-help window state and its retained backdrop.

mod input;
mod keyboard;
mod list;
mod shortcuts;
mod view;

pub(in crate::grid) use input::HelpCommand;

use crate::{
    grid::render::{Metrics, Panel},
    ui::ime::Composition,
};
use gpui::{App, AppContext, Entity, Modifiers};
use list::{HelpList, Scope};

/// Help owns its modal list and retained panels.
pub(in crate::grid) struct HelpWindow {
    /// One browsing session; absent while the popup is closed.
    pub(in crate::grid) list: Option<HelpList>,
    /// Interactive character keyboard for the editor's modal commands.
    keyboard: Option<keyboard::Keyboard>,
    /// Retained frame for the border and title.
    pub(in crate::grid) panel: Entity<Panel>,
    /// Clipped rows that move independently of the border.
    pub(in crate::grid) body: Entity<Panel>,
    /// Translucent text in the border title.
    pub(in crate::grid) dim_title: Entity<Panel>,
    /// Search prompt and buttons, drawn only during filtering.
    pub(in crate::grid) search: Entity<Panel>,
    /// Native preedit for the filter prompt.
    pub(in crate::grid) composition: Composition,
}

impl HelpWindow {
    /// Create a closed help window with shared `metrics` in the native application `cx`.
    pub(in crate::grid) fn new(metrics: Metrics, cx: &mut App) -> Self {
        Self {
            list: None,
            keyboard: None,
            panel: cx.new(|_| Panel::new(metrics)),
            body: cx.new(|_| Panel::new(metrics)),
            dim_title: cx.new(|_| Panel::new(metrics)),
            search: cx.new(|_| Panel::new(metrics)),
            composition: Composition::default(),
        }
    }

    /// Start help for `manager` or show the editor's character keyboard.
    /// `manager_available` controls the Explorer entries in the global list.
    pub(in crate::grid) fn open(&mut self, manager: bool, manager_available: bool) {
        self.list = Some(HelpList::new(manager, manager_available));
        self.keyboard = (!manager).then(keyboard::Keyboard::new);
    }

    /// Show picker help for `text_preview` and `choosing_file`.
    /// `manager_available` controls the Explorer entries in global help.
    pub(in crate::grid) fn open_file_picker(
        &mut self,
        manager_available: bool,
        text_preview: bool,
        choosing_file: bool,
    ) {
        use super::file_picker::{
            CHOOSE_PREVIEW_SHORTCUTS, CHOOSE_SHORTCUTS, SHORTCUTS, TEXT_PREVIEW_SHORTCUTS,
        };

        self.keyboard = None;
        let (bindings, title) = if text_preview {
            let bindings = if choosing_file {
                CHOOSE_PREVIEW_SHORTCUTS
            } else {
                TEXT_PREVIEW_SHORTCUTS
            };
            (bindings, "picker.preview_help_title")
        } else {
            let bindings = if choosing_file {
                CHOOSE_SHORTCUTS
            } else {
                SHORTCUTS
            };
            (bindings, "picker.help_title")
        };
        self.list = Some(HelpList::for_window(bindings, title, manager_available));
    }

    /// Discard the browsing session and its transient text.
    pub(in crate::grid) fn close(&mut self) {
        self.list = None;
        self.keyboard = None;
        self.composition.clear();
    }

    /// Whether the popup currently owns the workspace's input.
    pub(in crate::grid) fn is_open(&self) -> bool {
        self.list.is_some()
    }

    /// Whether this session was opened for the editor, including its global shortcuts page.
    pub(in crate::grid) fn is_editor_help(&self) -> bool {
        self.keyboard.is_some()
    }

    /// Whether the popup's filter prompt currently accepts native text.
    pub(in crate::grid) fn is_filtering(&self) -> bool {
        self.list.as_ref().is_some_and(HelpList::is_filtering)
    }

    /// Whether the editor's character keyboard is the visible help page.
    pub(in crate::grid) fn keyboard_visible(&self) -> bool {
        self.is_editor_help()
            && self
                .list
                .as_ref()
                .is_some_and(|list| list.scope() == Scope::Window)
    }

    /// Update `modifiers`, allowing taps only on the visible keyboard in an active window.
    pub(in crate::grid) fn update_keyboard_modifiers(
        &mut self,
        modifiers: Modifiers,
        active: bool,
    ) -> bool {
        let visible = self.keyboard_visible();
        self.keyboard
            .as_mut()
            .is_some_and(|keyboard| keyboard.update_modifiers(modifiers, active && visible))
            && visible
    }

    /// Disarm a tap when another popup action uses the held modifier.
    pub(in crate::grid) fn cancel_keyboard_modifier_tap(&mut self) {
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.cancel_modifier_tap();
        }
    }
}

#[cfg(test)]
mod tests;
