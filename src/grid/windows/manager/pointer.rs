//! File-manager row selection, double-click activation and scroll settling.

use super::ManagerWindow;
use crate::grid::render::{Metrics, Rect};
use gpui::MouseDownEvent;
use std::path::PathBuf;

/// A valid tree-row click, optionally requesting an editor file open.
pub(in crate::grid) struct ManagerSelection {
    /// Activated file; directories expand within the manager itself.
    pub open: Option<PathBuf>,
}

impl ManagerWindow {
    /// Hit-test `event` in `area` with `metrics`, using `first_click` for double-click recognition.
    /// `now` settles the selected row's continuous scroll position.
    pub(in crate::grid) fn press(
        &mut self,
        event: &MouseDownEvent,
        area: Rect,
        metrics: Metrics,
        first_click: Option<PathBuf>,
        now: f64,
    ) -> Option<ManagerSelection> {
        let (x, y) = metrics.cell(event.position);
        if Self::body(area).contains(x, y) {
            let body = Self::body(area);
            let index = (f32::from(event.position.y - metrics.origin.y) / metrics.lh
                - body.y as f32
                + self.scroll.position())
            .floor()
            .max(0.) as usize;
            if let Some(entry) = self.tree.entries.get(index) {
                let activate = event.click_count == 2 && first_click.as_ref() == Some(&entry.path);
                if event.click_count == 1 {
                    self.click = Some(entry.path.clone());
                }
                self.tree.selected = index;
                // Keep the clicked row under the pointer as a fractional scroll settles.
                let top = index - (y - body.y);
                self.scroll.snap(top, now);
                if activate && let Some(path) = self.tree.activate() {
                    return Some(ManagerSelection { open: Some(path) });
                }
                return Some(ManagerSelection { open: None });
            }
        }

        None
    }
}
