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
        let hit = self.tree.visible.hit(
            Self::body(area).pixels(metrics),
            event.position,
            metrics.lh,
            self.scroll.position(),
        )?;
        let entry = &self.tree.entries[hit.id];
        let activate = event.click_count == 2 && first_click.as_ref() == Some(&entry.path);
        if event.click_count == 1 {
            self.click = Some(entry.path.clone());
        }
        self.tree.selected = hit.id;
        self.scroll.snap(hit.aligned_first, now);
        let open = if activate { self.tree.activate() } else { None };
        Some(ManagerSelection { open })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    /// A row hit selects immediately; a second click opens only the original path.
    #[test]
    fn manager_double_click_checks_the_path_after_tree_expansion() -> anyhow::Result<()> {
        let project = tempfile::tempdir()?;
        let directory = project.path().join("folder");
        let file = directory.join("file.rs");
        std::fs::create_dir(&directory)?;
        std::fs::write(&file, "")?;
        let mut manager = ManagerWindow::new(project.path().into());
        let area = Rect {
            x: 3,
            y: 2,
            w: 25,
            h: 9,
        };
        let body = ManagerWindow::body(area);
        let metrics = Metrics {
            cw: 8.,
            lh: 16.,
            font_size: 14.,
            origin: point(px(3.), px(7.)),
        };
        let click = |manager: &mut ManagerWindow, row: usize, count| {
            let event = MouseDownEvent {
                position: metrics.at(body.x as f32 + 1., (body.y + row) as f32 + 0.5),
                click_count: count,
                ..Default::default()
            };
            let first = manager.take_click();
            manager
                .press(&event, area, metrics, first, 0.)
                .unwrap()
                .open
        };
        assert!(click(&mut manager, 0, 1).is_none());
        assert_eq!(manager.tree.visible.len(), 1);
        assert!(click(&mut manager, 0, 2).is_none());
        assert_eq!(manager.tree.visible.len(), 2);
        assert!(click(&mut manager, 0, 1).is_none());
        assert!(click(&mut manager, 1, 2).is_none());
        assert!(click(&mut manager, 1, 1).is_none());
        assert_eq!(click(&mut manager, 1, 2), Some(file));
        Ok(())
    }
}
