//! Route buffer management to the editor that owns the displayed file labels.

use super::{Workspace, editor_close::CloseTarget, layout::Focus};
use crate::grid::windows::editor::EditorWindow;
use gpui::{Context, Window};

/// Close action requested from the buffer Leader group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BufferClose {
    /// Close the selected file, retaining its editor window.
    Current,
    /// Keep the selected file and close its siblings.
    Others,
    /// Close file labels preceding the selected label.
    Left,
    /// Close file labels following the selected label.
    Right,
    /// Close the split and the buffers it owns.
    WithWindow,
}

/// Part of a window's buffer order relative to a retained reference label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BufferRange {
    /// Keep the reference label and close every other label.
    Others,
    /// Close labels before the reference label.
    Left,
    /// Close labels after the reference label.
    Right,
}

impl BufferRange {
    /// Find indices in `editor` covered by this range relative to buffer `reference`.
    pub(super) fn indices(
        self,
        editor: &EditorWindow,
        reference: u64,
    ) -> impl Iterator<Item = usize> {
        let reference = editor.buffer_index(reference);
        (0..editor.buffers().len()).filter(move |&index| {
            reference.is_some_and(|reference| match self {
                Self::Others => index != reference,
                Self::Left => index < reference,
                Self::Right => index > reference,
            })
        })
    }
}

impl Workspace {
    /// Return to the focused editor's previous file and refresh `window` through `cx`.
    pub(super) fn switch_alternate_buffer(
        &mut self,
        id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if id.is_none()
            && !self.modal_open()
            && let Some(project) = self.projects.active_project()
            && project.focus == Focus::Editor
            && project.editor.alternate_buffer_id().is_none()
            && let Some(path) = project
                .editor
                .alternate_path()
                .map(std::path::Path::to_owned)
        {
            self.finish_editor_input(window, cx);
            if let Err(error) = self.jump_to_file(&path, false, |doc| Some(doc.cursor)) {
                self.set_status(format!("{error:#}"));
            }
            self.sync(window, cx);
            return;
        }
        let Some(id) = self
            .projects
            .active_project()
            .filter(|project| project.focus == Focus::Editor && !self.modal_open())
            .and_then(|project| id.or_else(|| project.editor.alternate_buffer_id()))
        else {
            self.set_status(nanobug::i18n::text("editor.no_alternate_buffer").into());
            return;
        };
        self.finish_editor_input(window, cx);
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_mut();
        if project.editor.buffer_index(id).is_none() {
            self.set_status(nanobug::i18n::text("vim_bracket.no_buffer").into());
        } else if project.editor.select_buffer(id, now) {
            project.status.clear();
            self.last_cursor_space = None;
            self.sync(window, cx);
        }
    }

    /// Close the requested `scope` in the focused editor, resolving unsaved files first.
    pub(super) fn request_buffer_close(
        &mut self,
        scope: BufferClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .projects
            .active_project()
            .filter(|project| project.focus == Focus::Editor && !self.modal_open())
        else {
            return;
        };
        let id = project.editor.buffer().id;
        let target = match scope {
            BufferClose::Current => CloseTarget::Buffer(id),
            BufferClose::Others => CloseTarget::BufferRange(id, BufferRange::Others),
            BufferClose::Left => CloseTarget::BufferRange(id, BufferRange::Left),
            BufferClose::Right => CloseTarget::BufferRange(id, BufferRange::Right),
            BufferClose::WithWindow if project.split_count() > 1 => {
                CloseTarget::Split(project.active_editor_id())
            }
            BufferClose::WithWindow => return,
        };
        self.finish_editor_input(window, cx);
        self.request_close(target, window, cx);
        self.sync(window, cx);
    }

    /// Remove the labels in `range` after confirmation, keeping buffer `reference`.
    pub(super) fn finish_close_buffer_range(&mut self, reference: u64, range: BufferRange) {
        let editor = &mut self.projects.active_mut().editor;
        let ids = range
            .indices(editor, reference)
            .map(|index| editor.buffers()[index].id)
            .collect::<Vec<_>>();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        for id in ids {
            editor.remove_buffer_tab(id, now);
        }
    }
}
