//! Move plugin windows between projects and reattach guest views once they are published.

use super::{Action, Detail, ManagerWindow, Page, PluginCoordinator, SurfaceSnapshot, Workspace};
use crate::grid::windows::plugin_manager::ManagerSnapshot;
use serde::{Deserialize, Serialize};

/// Restorable host navigation for the plugin manager and one attached guest window.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(in crate::grid::workspace) struct PluginWindowsSnapshot {
    /// Whether plugin browsing was open.
    manager_open: bool,
    /// Plugin manager page, selection and navigation.
    manager: ManagerSnapshot,
    /// Optional published surface to reattach under the new project instance.
    surface: Option<SurfaceSnapshot>,
}

#[cfg(all(test, windows))]
mod tests;

impl PluginWindowsSnapshot {
    /// Give an explicitly requested file keyboard focus while retaining plugin navigation.
    pub(in crate::grid::workspace) fn focus_editor(&mut self) {
        self.manager_open = false;
        if let Some(surface) = &mut self.surface {
            surface.focused = false;
        }
    }
}

impl PluginCoordinator {
    /// Capture the ordinary plugin windows, bypassing temporary close and operation UI.
    fn snapshot_project_windows(&self) -> PluginWindowsSnapshot {
        if let Some(snapshot) = &self.closing_project_windows {
            return snapshot.clone();
        }
        let manager_open = self.open
            && self.formatting.choices.is_none()
            && self.language.is_none()
            && self.outline.is_none()
            && !self.file_busy
            && self.file_plan.is_none()
            && self.file_report.is_empty()
            && self.edit_plan.is_none()
            && self.edit_check.is_none();
        let surface = self
            .surface
            .key
            .as_ref()
            .map(|key| SurfaceSnapshot::new(key, self.surface.focused))
            .or_else(|| self.pending_project_surface.clone());
        PluginWindowsSnapshot {
            manager_open,
            manager: self.manager.project_snapshot(),
            surface,
        }
    }

    /// Preserve browsing and focus before a project close changes the visible window.
    pub(super) fn freeze_project_windows_for_close(&mut self) {
        if self.project_windows_scope.is_some() && self.closing_project_windows.is_none() {
            self.closing_project_windows = Some(self.snapshot_project_windows());
        }
    }

    /// Resume the previous navigation after a close is canceled.
    pub(in crate::grid::workspace) fn resume_project_windows_after_close(&mut self) {
        if let Some(snapshot) = self.closing_project_windows.take() {
            self.manager.restore_project_snapshot(snapshot.manager);
            self.open = snapshot.manager_open;
            self.surface.focused = self.surface.key.is_some()
                && snapshot.surface.is_some_and(|surface| surface.focused);
        }
    }
}

impl Workspace {
    /// Record the live plugin windows on the project that owns them before saving or switching.
    pub(in crate::grid::workspace) fn remember_project_plugin_windows(&mut self) {
        if let Some(scope) = self.plugins.project_windows_scope
            && let Some(project) = self.projects.project_by_scope_mut(scope)
        {
            project.plugin_windows = self.plugins.snapshot_project_windows();
        }
    }

    /// Swap native plugin window navigation when a directory project gains or loses activation.
    pub(super) fn sync_project_plugin_windows(&mut self) {
        let project = self
            .projects
            .active_project()
            .filter(|project| project.manager.is_some());
        let scope = project.map(|project| project.plugin_scope);
        if self.plugins.project_windows_scope == scope {
            return;
        }
        let snapshot = project
            .map(|project| project.plugin_windows.clone())
            .unwrap_or_default();
        self.remember_project_plugin_windows();
        self.plugins.project_windows_scope = scope;
        self.plugins.closing_project_windows = None;
        self.plugins.manager = ManagerWindow::default();
        self.plugins
            .manager
            .restore_project_snapshot(snapshot.manager);
        self.plugins.composition.clear();
        self.plugins.surface.clear_attachment();
        self.plugins.interaction = Default::default();
        self.plugins.pending_project_surface = snapshot.surface;
        self.plugins.open = snapshot.manager_open;
        if self.plugins.open {
            if self.plugins.manager.detail_page == Detail::Operations
                && let Some(namespace) = self.plugins.manager.detail.clone()
            {
                self.plugins.send(Action::ReviewToolOperations(namespace));
            } else if self.plugins.manager.page == Page::Marketplace {
                self.plugins.send(Action::OpenMarketplace);
            }
        }
    }

    /// Attach a restored window after the plugin publishes its view and report whether it joined.
    pub(super) fn reattach_project_surface(&mut self) -> bool {
        let (Some(scope), Some(snapshot)) = (
            self.plugins.project_windows_scope,
            self.plugins.pending_project_surface.as_ref(),
        ) else {
            return false;
        };
        let key = snapshot.key(scope);
        if let Some(surface) = self.plugins.view.surfaces.get(&key) {
            let focus = snapshot.focused && !self.modal_open();
            self.plugins.surface.attach(key, surface, focus);
            self.plugins.pending_project_surface = None;
            return true;
        }
        false
    }
}
