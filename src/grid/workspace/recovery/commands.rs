//! Ex access to local draft checkpoints in the editor workspace.

use super::{Draft, Record, Workspace, Writer};
use anyhow::{Context as _, ensure};
use gpui::{Context, Window};
use nanobug::{
    document::Document,
    i18n,
    vim::{Action, RecoveryCommand},
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

impl Workspace {
    /// Execute `command` using the active pane and Session for buffer and path resolution.
    pub(in crate::grid::workspace) fn ex_recovery(
        &mut self,
        command: RecoveryCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match command {
            RecoveryCommand::Preserve => self.preserve_draft(),
            RecoveryCommand::SwapName => self.draft_path(),
            RecoveryCommand::Recover { file, force } => {
                self.recover_buffer(&file, force, window, cx)
            }
        };
        match result {
            Ok(message) => {
                if self.input_mappings.ex_silence().is_none() {
                    self.set_status(String::new());
                }
                self.ex_output_message(&message, true);
            }
            Err(error) => self.set_status(format!("{error:#}")),
        }
    }

    /// Return this workspace's live recovery writer or explain why it is unavailable.
    fn recovery_writer(&self) -> anyhow::Result<Arc<Mutex<Writer>>> {
        self.recovery.writer.clone().with_context(|| {
            if let Some(error) = &self.recovery.unavailable {
                i18n::format("ex.recovery_error", &[("error", error)])
            } else {
                i18n::text("ex.recovery_disabled").into()
            }
        })
    }

    /// Capture the current shared text and immediately publish its checkpoint.
    fn preserve_draft(&mut self) -> anyhow::Result<String> {
        let writer = self.recovery_writer()?;
        let generation = self.recovery.next_generation();
        let project = self.projects.active_mut();
        project.refresh_shared_documents();
        let buffer = project.editor.buffer();
        ensure!(
            buffer.supports_shared_view() && buffer.vim.swapfile_enabled(),
            i18n::text("ex.recovery_disabled")
        );
        let document = &buffer.vim.doc;
        let id = document.buffer_number();
        let record = Record::new(&project.root, document);
        let draft = Draft {
            scope: record.scope.clone(),
            buffer: document.recovery_draft(),
        };
        let mut writer = writer.lock().expect("recovery writer");
        writer.generation = generation;
        writer.write(id, record, &draft)?;
        let path = writer.store.path(id);
        Ok(i18n::format(
            "ex.recovery_preserved",
            &[("path", &path.display())],
        ))
    }

    /// Report the actual checkpoint path for this buffer, if one exists.
    fn draft_path(&self) -> anyhow::Result<String> {
        let vim = self.projects.active().editor.vim();
        let writer = self.recovery_writer()?;
        let writer = writer.lock().expect("recovery writer");
        let path = writer.store.path(vim.doc.buffer_number());
        ensure!(
            vim.swapfile_enabled() && path.try_exists()?,
            i18n::text("ex.recovery_no_swap")
        );
        Ok(path.display().to_string())
    }

    /// Load a checkpoint for `file` or the current buffer, with optional dirty-buffer replacement.
    fn recover_buffer(
        &mut self,
        file: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<String> {
        let writer = self.recovery_writer()?;
        let project = self.projects.active_mut();
        project.refresh_shared_documents();
        let editor = &project.editor;
        let scope = project.root.clone();
        let current = editor.vim().doc.path.clone();
        let requested = if file.is_empty() {
            current.clone()
        } else {
            Some(self.file_operand(file).with_context(|| {
                i18n::format("editor.invalid_ex_argument", &[("argument", &file)])
            })?)
        };
        let candidate = {
            let writer = writer.lock().expect("recovery writer");
            let explicit = match requested.as_deref().filter(|_| !file.is_empty()) {
                Some(path) => writer.store.open(path)?,
                None => None,
            };
            if explicit.is_some() {
                explicit
            } else {
                writer.store.find(&scope, requested.as_deref())?
            }
        }
        .with_context(|| i18n::text("ex.recovery_not_found"))?;

        let draft = &candidate.draft.buffer;
        let destination = draft
            .path
            .as_ref()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
        {
            let project = self.projects.active();
            let editor = &project.editor;
            let target = match destination.as_deref() {
                Some(path) => project
                    .number_for_path(path)
                    .and_then(|id| project.session_buffer(id)),
                None if current.is_none() => Some(editor.buffer()),
                None => None,
            };
            if let Some(buffer) = target {
                ensure!(
                    buffer.supports_shared_view(),
                    i18n::text("ex.recovery_disabled")
                );
                ensure!(
                    force || !buffer.vim.doc.modified(),
                    i18n::text("ex.recovery_dirty")
                );
            }
        }
        if !candidate.is_owned() && draft.written_to_disk().unwrap_or(false) {
            return Ok(i18n::text("ex.recovery_already_saved").into());
        }
        let disk_matches = draft.matches_disk().unwrap_or(false);
        if let Some(path) = &destination {
            if current.as_ref() != Some(path)
                && !self
                    .jump_to_file(path, false, |doc| Some(doc.cursor))
                    .unwrap_or(false)
            {
                // A complete draft can still be read when the original file is unreadable or invalid.
                let mut document = Document::new("");
                document.set_ex_filename(Some(path.clone()));
                self.open_recovery_buffer(document);
            }
        } else if current.is_some() {
            self.open_recovery_buffer(Document::new(""));
        }
        self.apply_noswapfile_to_loaded();
        ensure!(
            self.projects
                .active()
                .editor
                .buffer()
                .supports_shared_view(),
            i18n::text("ex.recovery_disabled")
        );
        let mut applied = Ok(());
        self.run_vim(
            |vim, _| {
                applied = vim.doc.recover_draft(draft);
                Action::None
            },
            window,
            cx,
        );
        applied?;

        // Keep the abandoned checkpoint until a replacement is durable.
        if self.projects.active().editor.vim().swapfile_enabled() {
            self.preserve_draft()?;
            if !candidate.is_owned() {
                candidate.remove()?;
            }
        }
        Ok(i18n::text(if disk_matches {
            "ex.recovery_applied"
        } else {
            "ex.recovery_conflict"
        })
        .into())
    }

    /// Bring a prepared `document` into the active window while retaining its other labels.
    fn open_recovery_buffer(&mut self, document: Document) {
        let project = self.projects.active_mut();
        let editor = &mut project.editor;
        let index = editor.retain_document(document);
        let id = editor.buffers()[index].id;
        editor.select_buffer(id, self.scroll_epoch.elapsed().as_secs_f64());
        self.last_cursor_space = None;
    }

    /// Capture existing buffer numbers at the outermost `:noswapfile` dispatch.
    pub(in crate::grid::workspace) fn begin_noswapfile_dispatch(&mut self) -> bool {
        if !self.active_ex_window_modifiers.no_swapfile || self.noswapfile_buffers.is_some() {
            return false;
        }
        self.noswapfile_buffers = Some(self.loaded_recovery_buffers());
        true
    }

    /// Apply the modifier before a new buffer can run callbacks or publish a checkpoint.
    pub(in crate::grid::workspace) fn apply_noswapfile_to_loaded(&mut self) {
        let Some(before) = self.noswapfile_buffers.take() else {
            return;
        };
        let current = self.loaded_recovery_buffers();
        if !current.is_subset(&before) {
            for project in self.projects.iter_mut() {
                project.disable_swap_for_new_buffers(&before);
            }
        }
        self.noswapfile_buffers = Some(current);
    }

    /// Identify currently loaded editable buffer numbers within the workspace.
    fn loaded_recovery_buffers(&self) -> HashSet<u64> {
        self.projects
            .ordered()
            .flat_map(|project| {
                project
                    .session_buffer_entries()
                    .into_iter()
                    .filter_map(move |(id, buffer)| {
                        (buffer.supports_shared_view() && project.session_buffer_loaded(id))
                            .then_some(id)
                    })
            })
            .collect()
    }
}
