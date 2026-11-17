//! Prepare and activate destinations in this editor's cross-file jumplist.

use super::EditorWindow;
use anyhow::Context as _;
use nanobug::{document::Document, i18n, vim::Action};
use std::path::Path;

/// Destination checked before modifying any visible editor state.
enum Target {
    Existing(usize),
    Loaded(Box<super::Buffer>),
}

impl EditorWindow {
    /// Open `path` in this pane and record the departure after `resolve` validates its position.
    pub(in crate::grid) fn jump_file(
        &mut self,
        path: &Path,
        require_file: bool,
        now: f64,
        resolve: impl FnOnce(&Document) -> Option<usize>,
        shared: impl Fn(Option<u64>, Option<&Path>) -> Option<super::Buffer>,
    ) -> anyhow::Result<bool> {
        self.refresh_shared_documents();
        let target = self.prepare_jump(None, Some(path), require_file, &shared)?;
        let doc = self.target_document(&target);
        let Some(offset) = resolve(doc) else {
            return Ok(false);
        };
        if doc.opening != self.vim().doc.opening || offset != self.vim().doc.cursor {
            self.buffers[self.active].vim.record_jump();
        }
        self.activate_jump(target, now);
        self.buffers[self.active].vim.place_jump(offset);
        Ok(true)
    }

    /// Follow a cross-file mark at `(row, column)`, preferring its retained `opening`.
    pub(in crate::grid) fn jump_file_mark(
        &mut self,
        name: char,
        file: (u64, Option<&Path>),
        location: (usize, usize),
        linewise: bool,
        now: f64,
        shared: impl Fn(Option<u64>, Option<&Path>) -> Option<super::Buffer>,
    ) -> anyhow::Result<()> {
        self.refresh_shared_documents();
        let (opening, path) = file;
        let target = self.prepare_jump(Some(opening), path, true, &shared)?;
        self.buffers[self.active].vim.record_jump();
        self.activate_jump(target, now);
        self.edit(|vim, register| {
            vim.place_file_mark(name, location, linewise, register);
            Action::None
        });
        Ok(())
    }

    /// Move in the window's list by `count`, leaving the cursor in place if no target can load.
    pub(in crate::grid) fn visit_jump(
        &mut self,
        forward: bool,
        count: usize,
        now: f64,
        shared: impl Fn(Option<u64>, Option<&Path>) -> Option<super::Buffer>,
    ) -> bool {
        self.refresh_shared_documents();
        let destination = self
            .jump_history
            .visit(&self.vim().doc, forward, count, |entry| {
                let Some(target) = self
                    .prepare_jump(Some(entry.opening), entry.path.as_deref(), true, &shared)
                    .ok()
                else {
                    return Ok(None);
                };
                let offset = entry.offset(self.target_document(&target));
                Ok(Some((target, offset)))
            });
        let Ok(Some((target, offset))) = destination else {
            return false;
        };
        self.activate_jump(target, now);
        self.buffers[self.active].vim.place_jump(offset);
        true
    }

    /// Copy the source pane's history for a new split without sharing its navigation index.
    pub(in crate::grid) fn inherit_jumps(&mut self, source: &Self) {
        self.jump_history = source.jump_history.fork(&source.vim().doc, &self.vim().doc);
        for buffer in &mut self.buffers {
            buffer.vim.set_jump_history(self.jump_history.clone());
        }
    }

    /// Resolve a retained opening or fully read its file before activating the destination.
    fn prepare_jump(
        &self,
        opening: Option<u64>,
        path: Option<&Path>,
        require_file: bool,
        shared: &impl Fn(Option<u64>, Option<&Path>) -> Option<super::Buffer>,
    ) -> anyhow::Result<Target> {
        if let Some(opening) = opening
            && let Some(index) = self.buffers.iter().position(|buffer| {
                let doc = &buffer.vim.doc;
                doc.opening == opening || doc.buffer_number() == opening
            })
        {
            return Ok(Target::Existing(index));
        }
        let canonical = path.and_then(|path| path.canonicalize().ok());
        let path = canonical.as_deref().or(path);
        if let Some(index) = path.and_then(|path| {
            self.buffers
                .iter()
                .position(|buffer| buffer.vim.doc.path.as_deref() == Some(path))
        }) {
            return Ok(Target::Existing(index));
        }
        if let Some(buffer) = shared(opening, path) {
            return Ok(Target::Loaded(Box::new(buffer)));
        }
        let path = path.context(i18n::text("editor.missing_filename"))?;
        if require_file && !path.is_file() {
            anyhow::bail!(i18n::format(
                "vim_g.missing_file",
                &[("path", &path.display())]
            ));
        }
        let doc = Document::open(path)?;
        Ok(Target::Loaded(Box::new(super::Buffer::new(doc))))
    }

    /// Inspect a prepared `target` before recording or switching the current buffer.
    fn target_document<'a>(&'a self, target: &'a Target) -> &'a Document {
        match target {
            Target::Existing(index) => &self.buffers[*index].vim.doc,
            Target::Loaded(buffer) => &buffer.vim.doc,
        }
    }

    /// Select a prepared buffer at animation time `now`, retaining the previous buffer.
    fn activate_jump(&mut self, target: Target, now: f64) {
        let index = match target {
            Target::Existing(index) => index,
            Target::Loaded(buffer) => self.retain_buffer_view(*buffer),
        };
        self.select_buffer(self.buffers[index].id, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restore a closed file at its saved coordinates and skip an unavailable intervening file.
    #[test]
    fn jump_history_reopens_existing_files_and_skips_missing_ones() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first.txt");
        let missing = directory.path().join("missing.txt");
        let last = directory.path().join("last.txt");
        std::fs::write(&first, "first\n你好!\n")?;
        std::fs::write(&missing, "missing\n")?;
        std::fs::write(&last, "last\n")?;

        let mut editor = EditorWindow::new(Document::open(&first)?);
        editor.edit(|vim, _| {
            vim.set_cursor("first\n你".len());
            Action::None
        });
        let first_id = editor.buffer().id;
        editor.jump_file(&missing, true, 0., |_| Some(0), |_, _| None)?;
        let missing_id = editor.buffer().id;
        editor.jump_file(&last, true, 0., |_| Some(0), |_, _| None)?;
        editor.remove_buffer_tab(first_id, 0.);
        editor.remove_buffer_tab(missing_id, 0.);
        std::fs::remove_file(missing)?;

        assert!(editor.visit_jump(false, 1, 0., |_, _| None));
        assert!(
            editor
                .vim()
                .doc
                .path
                .as_ref()
                .unwrap()
                .ends_with("first.txt")
        );
        assert_eq!(editor.vim().doc.cursor, "first\n你".len());
        assert!(editor.visit_jump(true, 1, 0., |_, _| None));
        assert!(
            editor
                .vim()
                .doc
                .path
                .as_ref()
                .unwrap()
                .ends_with("last.txt")
        );
        Ok(())
    }
}
