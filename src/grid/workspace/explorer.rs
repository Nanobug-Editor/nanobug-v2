//! Route Explorer commands and reconcile open buffers after filesystem transfers.

use crate::grid::windows::manager::ManagerWindow;

use super::{Workspace, keyboard::PendingPrefix};
use crate::grid::windows::manager::ManagerCommand;
use gpui::{Context, Window};
use nanobug::animation::scroll::ScrollInput;
use std::time::Duration;

impl Workspace {
    /// Apply manager transfer `key`, guarding and updating buffers across all projects.
    pub(super) fn transfer(&mut self, key: &str) {
        let dirty_paths = self
            .projects
            .ordered()
            .flat_map(|project| project.editor.dirty_paths())
            .collect::<Vec<_>>();
        let active = &mut self.projects.active;
        let (changes, message) = active.manager.transfer(key, &active.root, &dirty_paths);
        if !changes.is_empty() {
            for project in self.projects.iter_mut() {
                project.editor.apply_path_changes(&changes);
            }
            self.last_cursor_space = None;
        }
        self.projects.active.status = message;
    }

    /// Interpret a manager `key` after a possible `prefix`, returning its viewport intent.
    /// `window` and `cx` schedule a standalone undo when the second key of `z` does not arrive.
    pub(super) fn manager_key(
        &mut self,
        key: &str,
        prefix: Option<char>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ScrollInput> {
        let height = ManagerWindow::body(self.layout.tree).h;
        if prefix == Some('z') {
            let inset = match key {
                "t" => Some(0),
                "z" => Some(height.saturating_sub(1) / 2),
                "b" => Some(height.saturating_sub(1)),
                _ => None,
            };
            if let Some(inset) = inset {
                return Some(ScrollInput::Position(
                    self.projects
                        .active
                        .manager
                        .tree
                        .selected
                        .saturating_sub(inset),
                ));
            }
            if key != "escape" {
                self.transfer("z");
            }
        }
        let (scroll, command) = self.projects.active.manager.key(key, prefix)?;
        match command {
            ManagerCommand::None => {}
            ManagerCommand::Open(path) => self.open(path),
            ManagerCommand::Status(message) => self.projects.active.status = message,
            ManagerCommand::Transfer(key) => self.transfer(key),
            ManagerCommand::Prefix(prefix) => {
                self.prefix = Some(PendingPrefix::Manager(prefix));
                if prefix == 'z' {
                    let delay = cx.background_executor().timer(Duration::from_millis(350));
                    self.projects.active.manager.paste_timer =
                        Some(cx.spawn_in(window, async move |weak, cx| {
                            delay.await;
                            let _ = cx.update(|window, cx| {
                                let _ = weak.update(cx, |workspace, cx| {
                                    if workspace.prefix == Some(PendingPrefix::Manager('z')) {
                                        workspace.prefix = None;
                                        workspace.transfer("z");
                                        workspace.repaint([ScrollInput::Reveal; 3], window, cx);
                                    }
                                });
                            });
                        }));
                }
            }
        }
        Some(scroll)
    }
}
