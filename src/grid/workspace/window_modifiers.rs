//! Apply Ex window modifiers within one native command dispatch.

mod dialogs;

use super::{Workspace, editor_splits::SplitAxis, projects::Project};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{WindowModifiers, WindowOrientation, WindowPlacement},
};

/// The geometry for the next native split requested by the active Ex command.
pub(super) struct SplitPosition {
    pub(super) axis: SplitAxis,
    pub(super) first: bool,
    pub(super) outer: bool,
}

impl Workspace {
    /// Dispatch `command` with merged geometry and restore the caller's modifier context.
    pub(super) fn execute_window_modified(
        &mut self,
        modifiers: WindowModifiers,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let merged = self.active_ex_window_modifiers.overlay(modifiers);
        if let Some(address) = &merged.tab
            && self
                .projects
                .active_project()
                .and_then(|project| tab_insertion(address, project))
                .is_none()
        {
            self.set_status(i18n::format(
                "editor.invalid_ex_argument",
                &[("argument", address)],
            ));
            return;
        }
        let prior = std::mem::replace(&mut self.active_ex_window_modifiers, merged);
        if !self.active_ex_window_modifiers.browse || !self.browse_ex_command(command, window, cx) {
            self.run_window_modified_command(command, window, cx);
        }
        self.active_ex_window_modifiers = prior;
    }

    /// Resolve the current modifier's insertion point when a command requests a new window.
    pub(super) fn ex_tab_insertion(&self) -> Option<usize> {
        let address = self.active_ex_window_modifiers.tab.as_deref()?;
        tab_insertion(address, self.projects.active())
    }

    /// Resolve the requested placement and orientation of a new split.
    pub(super) fn ex_split_position(&self, axis: SplitAxis, new_first: bool) -> SplitPosition {
        let modifiers = &self.active_ex_window_modifiers;
        let axis = match modifiers.orientation {
            Some(WindowOrientation::Horizontal) => SplitAxis::Rows,
            Some(WindowOrientation::Vertical) => SplitAxis::Columns,
            None => axis,
        };
        let (first, outer) = match modifiers.placement {
            Some(WindowPlacement::Before) => (true, false),
            Some(WindowPlacement::After) => (false, false),
            Some(WindowPlacement::First) => (true, true),
            Some(WindowPlacement::Last) => (false, true),
            None => (new_first, false),
        };
        SplitPosition { axis, first, outer }
    }
}

/// Convert Ex's after-page count to the zero-based index of the new Tabpage.
fn tab_insertion(address: &str, project: &Project) -> Option<usize> {
    let current = project.active_page() + 1;
    let count = project.page_count();
    let position = match address {
        "" | "." => current,
        "$" => count,
        relative if relative.starts_with(['+', '-']) => {
            let magnitude = relative[1..]
                .parse::<usize>()
                .ok()
                .or(Some(1).filter(|_| relative.len() == 1))?;
            if relative.starts_with('+') {
                current.checked_add(magnitude)?
            } else {
                current.checked_sub(magnitude)?
            }
        }
        number => number.parse().ok()?,
    };
    (position <= count).then_some(position)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use gpui::AppContext;
    use nanobug::plugins::session::Options;
    use std::{fs, time::Duration};

    /// Choose an existing path in the grid picker and keep dirty buffers on canceling :confirm qa.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_browse_and_confirm_preserve_the_originating_editor() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        let file = root.join("file with spaces.txt");
        fs::write(root.join("README.md"), "initial\n")?;
        fs::write(&file, "selected\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temporary = temporary;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.sync(window, cx);
                        let origin = workspace.projects.active().active_editor_id();
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("browse edit", register),
                            window,
                            cx,
                        );
                        ensure!(
                            workspace
                                .file_picker
                                .as_ref()
                                .is_some_and(|picker| picker.choosing_file())
                        );
                        workspace.finish_file_picker(Some(file.clone()), window, cx);
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == origin);
                        ensure!(project.editor.vim().doc.path == Some(file.canonicalize()?));
                        ensure!(project.editor.vim().doc.text == "selected\n");
                        workspace.run_vim(
                            |vim, _| {
                                vim.doc.replace(0..8, "dirty", true);
                                nanobug::vim::Action::None
                            },
                            window,
                            cx,
                        );
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("qa", register),
                            window,
                            cx,
                        );
                        ensure!(workspace.editor_close.is_none());
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("confirm qa", register),
                            window,
                            cx,
                        );
                        ensure!(workspace.editor_close.is_some());
                        workspace.cancel_editor_close(window, cx);
                        ensure!(workspace.projects.active().editor.vim().doc.text == "dirty\n");
                        ensure!(fs::read_to_string(file)? == "selected\n");
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Track an active viewport's movement, then align a peer which has been scrolled independently.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_syncbind_aligns_smooth_split_viewports() -> Result<()> {
        use nanobug::animation::scroll::ScrollInput;
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "one row\n".repeat(240))?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temporary = temporary;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(1200.), gpui::px(880.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.sync(window, cx);
                        let first = workspace.projects.active().active_editor_id();
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("vsplit", register),
                            window,
                            cx,
                        );
                        let project = workspace.projects.active();
                        let second = project.active_editor_id();
                        ensure!(first != second, "{}", project.status);
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("setlocal scrollbind", register),
                            window,
                            cx,
                        );
                        workspace
                            .projects
                            .active_mut()
                            .focus_editor_by_id(first, 0.);
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("setlocal scb", register),
                            window,
                            cx,
                        );
                        workspace.repaint(
                            [
                                ScrollInput::Advance,
                                ScrollInput::Wheel(14),
                                ScrollInput::Advance,
                            ],
                            window,
                            cx,
                        );
                        let project = workspace.projects.active();
                        ensure!(project.editor.scrollbind_target() == Some(14));
                        ensure!(
                            project.editor_by_id(second).unwrap().scrollbind_target() == Some(14)
                        );
                        ensure!(
                            project
                                .editor_by_id(second)
                                .unwrap()
                                .buffer()
                                .scroll
                                .is_active()
                        );
                        let area = project
                            .splits()
                            .rect(second, workspace.visible_editor_region())
                            .unwrap();
                        let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                        let peer = workspace
                            .projects
                            .active_mut()
                            .editor_by_id_mut(second)
                            .unwrap();
                        peer.track_view(
                            area,
                            false,
                            &workspace.spacing,
                            [ScrollInput::Wheel(7), ScrollInput::Advance],
                            now,
                        );
                        ensure!(peer.scrollbind_target() == Some(21));
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("syncbind", register),
                            window,
                            cx,
                        );
                        let project = workspace.projects.active();
                        ensure!(project.active_editor_id() == first);
                        ensure!(
                            project.editor_by_id(second).unwrap().scrollbind_target() == Some(14)
                        );
                        Ok(())
                    })
                })?
            },
        )
    }

    /// Verify relative/outer placement and a Tabpage projection through native Ex dispatch.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_window_modifiers_place_panes_and_numbered_tabpages() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("README.md"), "same document\n")?;
        fs::write(root.join("next.txt"), "other document\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temporary.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                let _temporary = temporary;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(gpui::size(gpui::px(1200.), gpui::px(880.)));
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.sync(window, cx);
                        let mut ex = |workspace: &mut Workspace, text: &str| {
                            workspace.run_vim(
                                |vim, register| vim.execute_ex_line(text, register),
                                window,
                                cx,
                            );
                            workspace.sync(window, cx);
                        };
                        let start = workspace.projects.active().active_editor_id();
                        ex(workspace, "rightbelow vsplit");
                        let project = workspace.projects.active();
                        let right = project.active_editor_id();
                        ensure!(project.splits().ids() == [start, right]);
                        ex(workspace, "vertical resize 20");
                        let area = workspace.visible_editor_region();
                        let rect = workspace
                            .projects
                            .active()
                            .splits()
                            .rect(right, area)
                            .unwrap();
                        ensure!(rect.w == 22);
                        ex(workspace, "aboveleft split");
                        let project = workspace.projects.active();
                        let above = project.active_editor_id();
                        ensure!(project.splits().ids() == [start, above, right]);
                        let area = workspace.visible_editor_region();
                        let upper = project.splits().rect(above, area).unwrap();
                        let lower = project.splits().rect(right, area).unwrap();
                        ensure!(upper.x == lower.x && upper.y < lower.y);
                        ex(workspace, "vertical topleft new");
                        let project = workspace.projects.active();
                        let left = project.active_editor_id();
                        let area = workspace.visible_editor_region();
                        let leading = project.splits().rect(left, area).unwrap();
                        ensure!(leading.x == area.x && leading.y == area.y && leading.h == area.h);
                        ex(workspace, "horizontal botright new");
                        let project = workspace.projects.active();
                        let bottom = project.active_editor_id();
                        let area = workspace.visible_editor_region();
                        let trailing = project.splits().rect(bottom, area).unwrap();
                        ensure!(
                            trailing.x == area.x && trailing.w == area.w && trailing.y > area.y
                        );
                        let number = project.editor.vim().doc.buffer_number();
                        ex(workspace, "0tab split");
                        let project = workspace.projects.active();
                        ensure!(project.page_count() == 2 && project.active_page() == 0);
                        ensure!(
                            project.split_count() == 1
                                && project.editor.vim().doc.buffer_number() == number
                        );
                        ex(workspace, "$tab help");
                        let project = workspace.projects.active();
                        ensure!(project.page_count() == 3 && project.active_page() == 2);
                        ensure!(project.editor.vim().is_help());
                        ex(workspace, "tab drop README.md next.txt");
                        let project = workspace.projects.active();
                        ensure!(project.page_count() == 5 && project.active_page() == 3);
                        ensure!(
                            project
                                .editor
                                .vim()
                                .doc
                                .path
                                .as_deref()
                                .is_some_and(|path| path.ends_with("README.md"))
                        );
                        ensure!(project.tabpage_editors(4).any(|editor| {
                            editor
                                .vim()
                                .doc
                                .path
                                .as_deref()
                                .is_some_and(|path| path.ends_with("next.txt"))
                        }));
                        ensure!(workspace.active_ex_window_modifiers == WindowModifiers::default());
                        Ok(())
                    })
                })?
            },
        )
    }
}
