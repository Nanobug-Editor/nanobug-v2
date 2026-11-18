//! Apply Ex list commands in Session order and retain a version-checked undo receipt.

use super::{Workspace, editor_splits::PaneId};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    vim::{Action, ArgumentCommand, BatchScope, Vim},
};
use std::collections::BTreeMap;

/// Target selected once for a list command, with its one-based Ex address.
#[derive(Clone, Copy)]
enum Target {
    Argument(usize),
    Buffer(u64),
    Window(PaneId),
    Tabpage(usize),
}

/// Actual text changes of one completed or partially completed batch.
#[derive(Default)]
pub(super) struct BatchUndo {
    /// Stable buffer number to text before the first change and the last seen revision.
    edits: BTreeMap<u64, (String, u64)>,
}

impl Workspace {
    /// Repeat `commands` over the objects of `scope` in `addresses`, stopping at the first error.
    pub(super) fn run_ex_batch(
        &mut self,
        scope: BatchScope,
        addresses: &str,
        commands: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ex_batch_active {
            self.set_status(i18n::text("ex.batch_structure").into());
            return;
        }
        let project = self.projects.active();
        let session = project.plugin_scope;
        let current_buffer = project.editor.vim().doc.buffer_number();
        let (targets, current) = match scope {
            BatchScope::Arguments => (
                (1..=project.arguments.len(project.active_editor_id()))
                    .map(|index| (index as u64, Target::Argument(index)))
                    .collect::<Vec<_>>(),
                project.arguments.index(project.active_editor_id()) as u64 + 1,
            ),
            BatchScope::Buffers => (
                project
                    .session_buffer_entries()
                    .into_iter()
                    .filter(|(_, buffer)| buffer.supports_shared_view())
                    .map(|(number, _)| (number, Target::Buffer(number)))
                    .collect(),
                current_buffer,
            ),
            BatchScope::Windows => {
                let mut current = 1;
                let targets = project
                    .page_editors()
                    .enumerate()
                    .map(|(index, (pane, _))| {
                        let address = index as u64 + 1;
                        if pane == project.active_editor_id() {
                            current = address;
                        }
                        (address, Target::Window(pane))
                    })
                    .collect();
                (targets, current)
            }
            BatchScope::Tabpages => (
                (0..project.page_count())
                    .map(|page| (page as u64 + 1, Target::Tabpage(page)))
                    .collect(),
                project.active_page() as u64 + 1,
            ),
        };
        let last = targets.last().map_or(0, |(address, _)| *address);
        let Some((first, last)) = batch_bounds(addresses, current, last) else {
            self.set_status(i18n::text("ex.batch_range").into());
            return;
        };
        let mut undo = BatchUndo::default();
        self.ex_batch_active = true;
        for (_, target) in targets
            .into_iter()
            .filter(|(address, _)| (first..=last).contains(address))
        {
            if !self.select_batch_target(target, window, cx) {
                break;
            }
            for line in Vim::ex_statements(commands).filter(|line| !line.is_empty()) {
                if !self.execute_batch_line(line, &mut undo, window, cx) {
                    break;
                }
            }
            if !self.projects.active().status.is_empty() {
                break;
            }
        }
        self.ex_batch_active = false;
        if !undo.edits.is_empty() {
            self.ex_batch_undo.insert(session, undo);
        }
    }

    /// Reveal a batch target in its editor without opening extra splits.
    fn select_batch_target(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        self.set_status(String::new());
        match target {
            Target::Argument(index) => self.argument_list_command(
                ArgumentCommand::At,
                Some(index),
                String::new(),
                window,
                cx,
            ),
            Target::Buffer(number) => {
                if !self
                    .projects
                    .active_mut()
                    .select_session_buffer(number, now)
                {
                    self.set_status(i18n::text("vim_bracket.no_buffer").into());
                }
            }
            Target::Window(pane) => {
                self.projects.active_mut().focus_editor_by_id(pane, now);
            }
            Target::Tabpage(page) => {
                self.projects.active_mut().select_page(page, now);
            }
        }
        self.activate_input_mappings();
        self.projects.active().status.is_empty()
    }

    /// Execute one statement, collecting text only if that buffer actually changed.
    fn execute_batch_line(
        &mut self,
        line: &str,
        undo: &mut BatchUndo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let project = self.projects.active_mut();
        project.refresh_shared_documents();
        let doc = &project.editor.vim().doc;
        let number = doc.buffer_number();
        let revision = doc.revision;
        let before = doc.text.clone();
        let edit = project
            .editor
            .edit(|vim, register| vim.execute_ex_line(line, register));
        let error = project
            .editor
            .vim()
            .ex_failed()
            .then(|| project.editor.vim().message.clone());
        if project.editor.vim().substitute_confirmation_active() {
            project.editor.edit(|vim, _| {
                vim.cancel_pending();
                Action::None
            });
            self.set_status(i18n::text("ex.batch_confirm").into());
        } else if let Some(error) = error {
            self.set_status(error);
        } else if disrupts_batch(&edit.action) {
            self.set_status(i18n::text("ex.batch_structure").into());
        } else {
            self.apply_edit(edit, window, cx);
        }
        let doc = &self.projects.active().editor.vim().doc;
        if doc.buffer_number() == number && doc.revision != revision && doc.text != before {
            let entry = undo.edits.entry(number).or_insert((before, revision));
            entry.1 = doc.revision;
            if doc.text == entry.0 {
                undo.edits.remove(&number);
            }
        }
        self.projects.active().status.is_empty()
    }

    /// Restore the last batch in memory after checking every modified buffer's revision.
    pub(super) fn undo_ex_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.projects.active_mut();
        project.refresh_shared_documents();
        let session = project.plugin_scope;
        let Some(undo) = self.ex_batch_undo.get(&session) else {
            self.set_status(i18n::text("ex.no_batch").into());
            return;
        };
        let mut targets = Vec::new();
        for (pane, index) in project.session_buffers() {
            let doc = &project.editor_by_id(pane).expect("retained pane").buffers()[index]
                .vim
                .doc;
            if let Some((text, version)) = undo.edits.get(&doc.buffer_number())
                && doc.revision == *version
            {
                targets.push((pane, index, text.clone()));
            }
        }
        if targets.len() != undo.edits.len() {
            self.set_status(i18n::text("ex.batch_stale").into());
            return;
        }
        self.ex_batch_undo.remove(&session);
        for (pane, index, text) in targets {
            let editor = self
                .projects
                .active_mut()
                .editor_by_id_mut(pane)
                .expect("validated pane");
            let edit = editor.edit_buffer(index, |vim, _| {
                nanobug::plugins::edits::apply(&mut vim.doc, &text);
                Action::None
            });
            self.apply_edit(edit, window, cx);
        }
        self.set_status(String::new());
    }
}

/// Address the full list for an empty range and support Ex's current and last markers.
fn batch_bounds(range: &str, current: u64, last: u64) -> Option<(u64, u64)> {
    if matches!(range, "" | "%") {
        return Some((1, last));
    }
    let parse = |word: &str| match word.trim() {
        "." => Some(current),
        "$" => Some(last),
        number => number.parse().ok(),
    };
    let (start, end) = range.split_once(',').unwrap_or((range, range));
    let start = parse(start)?;
    let end = parse(end)?;
    (start > 0 && end >= start && end <= last).then_some((start, end))
}

/// List commands keep their iteration targets valid until the last visit.
fn disrupts_batch(action: &Action) -> bool {
    matches!(
        action,
        Action::Arguments { .. }
            | Action::RunBatch { .. }
            | Action::UndoBatch
            | Action::Close(_)
            | Action::CloseWindow(_)
            | Action::QuitAll(_)
            | Action::QuitWithCode(_)
            | Action::Restart { .. }
            | Action::SaveClose
            | Action::WriteFile { close: true, .. }
            | Action::WriteQuitAll
            | Action::DeleteBuffer { .. }
            | Action::Open(_)
            | Action::OpenFile(..)
            | Action::OpenFileMark { .. }
            | Action::EmptyBuffer
            | Action::EmptySplit(_)
            | Action::Split { .. }
            | Action::Only
            | Action::WindowCommand { .. }
            | Action::TabNew(_)
            | Action::TabNavigate(..)
            | Action::TabEndpoint(_)
            | Action::TabMove(_)
            | Action::TabClose
            | Action::TabOnly
            | Action::Source { .. }
            | Action::Runtime { .. }
            | Action::SelectBuffer(_)
            | Action::NavigateBuffer { .. }
            | Action::ListBuffers
            | Action::ListTabs
            | Action::ShowListing(_)
            | Action::Sign(
                nanobug::vim::SignCommand::List { .. }
                    | nanobug::vim::SignCommand::ListPlaced { .. }
                    | nanobug::vim::SignCommand::Jump { .. }
            )
            | Action::EditableListing(_)
            | Action::IncludeSplit { .. }
            | Action::Files(_)
            | Action::BufferLifecycle(_)
            | Action::SessionExport(_)
            | Action::UndoFile { .. }
            | Action::WindowModified { .. }
            | Action::ExState { .. }
            | Action::MessageModified { .. }
            | Action::WindowExtras(_)
            | Action::Shell(_)
            | Action::RegisterEx(_)
            | Action::ExPipeline(_)
            | Action::UserEx { .. }
            | Action::Autocmd { .. }
            | Action::NoAutocmd(_)
            | Action::ClipboardEx
            | Action::FinishSource
            | Action::ListScripts
            | Action::EditSourcedScript { .. }
            | Action::Help(_)
            | Action::HelpTags { .. }
            | Action::CheckHealth(_)
            | Action::TerminalOpen(_)
            | Action::VimMenu { .. }
            | Action::Filter { .. }
            | Action::Sleep(_)
            | Action::Format(_)
            | Action::PreviewOpen(_)
            | Action::PreviewClose
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Result, ensure};
    use nanobug::plugins::session::Options;
    use std::fs;

    /// A confirmation request stops before another target can receive a hidden prompt.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_ex_batch_stops_before_leaving_a_confirmation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        fs::create_dir(&root)?;
        fs::write(root.join("a.txt"), "one\none\n")?;
        fs::write(root.join("b.txt"), "one\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            root,
            options,
            |_| {},
            async move |handle, app| {
                app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("args a.txt b.txt", register),
                            window,
                            cx,
                        );
                        workspace.run_vim(
                            |vim, register| vim.execute_ex_line("argdo %s/one/two/gc", register),
                            window,
                            cx,
                        );
                        let project = workspace.projects.active();
                        ensure!(project.status == i18n::text("ex.batch_confirm"));
                        ensure!(project.editor.vim().doc.text == "one\none\n");
                        ensure!(!project.editor.vim().substitute_confirmation_active());
                        ensure!(project.session_buffers().iter().all(|(pane, index)| {
                            let buffer = &project.editor_by_id(*pane).unwrap().buffers()[*index];
                            buffer
                                .vim
                                .doc
                                .path
                                .as_ref()
                                .is_none_or(|path| !path.ends_with("b.txt"))
                        }));
                        Ok(())
                    })
                })?
            },
        )
    }
}
