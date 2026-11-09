//! Shortcut-help window state and its retained backdrop.

mod backdrop;
mod input;
mod list;
mod shortcuts;
mod view;

pub(in crate::grid) use input::HelpCommand;
pub(in crate::grid) use view::help_outline;

use crate::{
    grid::render::{Metrics, Panel},
    ui::ime::Composition,
};
use gpui::{App, AppContext, Bounds, Entity, Pixels, RenderImage, Size};
use list::HelpList;
use std::sync::Arc;

/// Help owns its modal list, retained panels and cached backdrop.
pub(in crate::grid) struct HelpWindow {
    /// One browsing session; absent while the popup is closed.
    pub(in crate::grid) list: Option<HelpList>,
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
    /// Workspace image retained while the popup's placement and viewport are unchanged.
    backdrop: Option<HelpBackdrop>,
}

impl HelpWindow {
    /// Create a closed help window with shared `metrics` in the native application `cx`.
    pub(in crate::grid) fn new(metrics: Metrics, cx: &mut App) -> Self {
        Self {
            list: None,
            panel: cx.new(|_| Panel::new(metrics)),
            body: cx.new(|_| Panel::new(metrics)),
            dim_title: cx.new(|_| Panel::new(metrics)),
            search: cx.new(|_| Panel::new(metrics)),
            composition: Composition::default(),
            backdrop: None,
        }
    }

    /// Start a fresh session for the editor or, when `manager` is true, Explorer.
    pub(in crate::grid) fn open(&mut self, manager: bool) {
        self.list = Some(HelpList::new(manager));
    }

    /// Discard the browsing session and its transient text.
    pub(in crate::grid) fn close(&mut self) {
        self.list = None;
        self.composition.clear();
    }

    /// Whether the popup currently owns the workspace's input.
    pub(in crate::grid) fn is_open(&self) -> bool {
        self.list.is_some()
    }

    /// Whether the popup's filter prompt currently accepts native text.
    pub(in crate::grid) fn is_filtering(&self) -> bool {
        self.list.as_ref().is_some_and(HelpList::is_filtering)
    }
}

/// One cached, window-local backdrop while shortcut help is open.
struct HelpBackdrop {
    /// Logical size of the window the image covers.
    viewport: Size<Pixels>,
    /// Rounded popup bounds reserved in the image.
    outline: Bounds<Pixels>,
    /// Downsampled, blurred workspace texture.
    image: Arc<RenderImage>,
}

#[cfg(test)]
mod tests;
