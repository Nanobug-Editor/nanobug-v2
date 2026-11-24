//! Project Quickfix lists, per-window location lists and live diagnostic result windows.

mod filter;
mod history;
mod model;
mod window;

use super::Workspace;
use crate::grid::windows::editor::ResultList;
use gpui::{Context, Window};
use model::List;
pub(super) use model::{Entry, Lists};
use nanobug::{i18n, vim::QuickfixCommand};
use nanobug_plugin_sdk::DiagnosticSeverity;

impl Workspace {
    /// Execute an Ex Quickfix or location list command in the active source window's scope.
    pub(super) fn quickfix_command(
        &mut self,
        location: bool,
        command: QuickfixCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let kind = project.result_list_kind(location);
        if location
            && project.result_lists.current(kind).is_none()
            && !matches!(
                command,
                QuickfixCommand::Close
                    | QuickfixCommand::History(None)
                    | QuickfixCommand::Current(_)
            )
        {
            self.set_status(i18n::text("quickfix.no_location_list").into());
            return;
        }
        match command {
            QuickfixCommand::Older(_) | QuickfixCommand::Newer(_) | QuickfixCommand::History(_) => {
                self.result_history(kind, command, window, cx);
            }
            QuickfixCommand::Bracket { forward, count } => {
                let active = project.editor.buffer().list_owner().map(|(kind, _)| kind);
                let trouble = active
                    .into_iter()
                    .chain(project.page_editors().filter_map(|(_, editor)| {
                        editor.buffer().list_owner().map(|(kind, _)| kind)
                    }))
                    .find(|kind| {
                        matches!(
                            kind,
                            ResultList::Diagnostics | ResultList::BufferDiagnostics(_)
                        )
                    });
                self.navigate_result(
                    trouble.unwrap_or(kind),
                    if forward {
                        QuickfixCommand::Next(count)
                    } else {
                        QuickfixCommand::Previous(count)
                    },
                    window,
                    cx,
                );
            }
            QuickfixCommand::CurrentLine => {
                let current = project
                    .editor
                    .vim()
                    .doc
                    .location(project.editor.vim().doc.cursor)
                    .0;
                self.navigate_result(
                    kind,
                    QuickfixCommand::Current(Some(current + 1)),
                    window,
                    cx,
                );
            }
            QuickfixCommand::Open(size) => self.open_result_list(kind, size, window, cx),
            QuickfixCommand::Close => self.close_result_list(kind, window, cx),
            QuickfixCommand::Window(size) => {
                if project
                    .result_lists
                    .current(kind)
                    .is_some_and(|list| !list.entries.is_empty())
                {
                    if project.result_pane(kind).is_none() {
                        self.open_result_list(kind, size, window, cx);
                    }
                } else {
                    self.close_result_list(kind, window, cx);
                }
            }
            QuickfixCommand::List { from, to } => self.list_entries(kind, from, to, window, cx),
            QuickfixCommand::ListNext(count) => {
                let index = project
                    .result_lists
                    .current(kind)
                    .map_or(1, |list| list.current + 1);
                self.list_entries(
                    kind,
                    Some(index as isize),
                    Some(index.saturating_add(count).min(isize::MAX as usize) as isize),
                    window,
                    cx,
                );
            }
            command => self.navigate_result(kind, command, window, cx),
        }
    }

    /// List, restore or step through previously created result lists in this scope.
    fn result_history(
        &mut self,
        kind: ResultList,
        command: QuickfixCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let history = self.projects.active_mut().result_lists.history(kind);
        if matches!(command, QuickfixCommand::History(None)) {
            let rows = history.description(true);
            self.ex_output_listing(&rows, window, cx);
            return;
        }
        let selected = history.select(command);
        let description = selected.map(|_| history.description(false));
        let row = history.current().map(|list| list.current);
        self.update_result_list_windows(kind, row);
        match description {
            Ok(message) => {
                self.set_status(String::new());
                self.ex_output_message(&message, false);
            }
            Err(key) => self.set_status(i18n::text(key).into()),
        }
        self.sync(window, cx);
    }

    /// Create a separate filtered snapshot; preceding history and the source editor stay accessible.
    pub(super) fn filter_result_list(
        &mut self,
        location: bool,
        title: String,
        keep: impl Fn(&Entry) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let kind = project.result_list_kind(location);
        let list = match project.result_lists.current(kind) {
            Some(list) => list.filtered(title, keep),
            None => List {
                title,
                ..List::default()
            },
        };
        self.projects
            .active_mut()
            .result_lists
            .history(kind)
            .push(list);
        self.update_result_list_windows(kind, Some(0));
        self.sync(window, cx);
    }

    /// Print an optionally restricted range of rows through the ordinary Ex output pipeline.
    fn list_entries(
        &mut self,
        kind: ResultList,
        from: Option<isize>,
        to: Option<isize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.active();
        let Some(list) = project
            .result_lists
            .current(kind)
            .filter(|list| !list.entries.is_empty())
        else {
            self.set_status(i18n::text("quickfix.empty").into());
            return;
        };
        let len = list.entries.len() as isize;
        let number = |n: isize| if n < 0 { len + 1 + n } else { n };
        let range = number(from.unwrap_or(1))..=number(to.unwrap_or(len));
        let rows = list
            .entries
            .iter()
            .enumerate()
            .filter(|(i, _)| range.contains(&(*i as isize + 1)))
            .map(|(i, entry)| format!("{} {}", i + 1, entry.label(&project.root)))
            .collect::<Vec<_>>()
            .join("\n");
        self.ex_output_listing(&rows, window, cx);
    }

    /// Replace the project Quickfix or source window's location list with diagnostic snapshots.
    pub(super) fn set_diagnostic_list(
        &mut self,
        location: bool,
        open: bool,
        severity: Option<DiagnosticSeverity>,
        title: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let source = project.result_source_pane();
        let buffer = project
            .editor_by_id(source)
            .unwrap()
            .vim()
            .doc
            .buffer_number();
        let kind = project.result_list_kind(location);
        let mut entries = self.diagnostic_entries(location.then_some(buffer));
        entries.retain(|entry| severity.is_none_or(|value| value == entry.severity));
        let anchors = entries
            .iter()
            .map(|entry| {
                let doc = &project.session_buffer(entry.buffer)?.vim.doc;
                (doc.revision == entry.revision && doc.path == entry.path)
                    .then(|| doc.anchor(entry.offset))
            })
            .collect();
        let project = self.projects.active_mut();
        let history = project.result_lists.history(kind);
        let list = List {
            title: title.unwrap_or_else(|| "Diagnostics".into()),
            entries,
            anchors,
            current: 0,
        };
        if history.update_diagnostics(location, open, list) {
            let row = history.current().map(|list| list.current);
            self.update_result_list_windows(kind, row);
        }
        if open {
            self.quickfix_command(location, QuickfixCommand::Window(None), window, cx);
        } else {
            self.sync(window, cx);
        }
    }

    /// Toggle the Quickfix or source window's location list with LazyVim's list keys.
    pub(super) fn toggle_quickfix(
        &mut self,
        location: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let kind = project.result_list_kind(location);
        if project.result_pane(kind).is_some() {
            self.close_result_list(kind, window, cx)
        } else {
            self.quickfix_command(location, QuickfixCommand::Open(None), window, cx)
        }
    }

    /// Resolve a buffer-specific diagnostics window relative to the last source pane.
    fn diagnostic_list_kind(&self, buffer: bool) -> Option<ResultList> {
        let project = self.projects.active_project()?;
        if buffer
            && let Some((kind @ ResultList::BufferDiagnostics(_), _)) =
                project.editor.buffer().list_owner()
        {
            return Some(kind);
        }
        Some(if buffer {
            ResultList::BufferDiagnostics(
                project
                    .editor_by_id(project.result_source_pane())?
                    .vim()
                    .doc
                    .buffer_number(),
            )
        } else {
            ResultList::Diagnostics
        })
    }

    /// Open a project or current-buffer list bound to live diagnostic changes.
    pub(super) fn show_diagnostics(
        &mut self,
        current_buffer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(kind) = self.diagnostic_list_kind(current_buffer) {
            self.refresh_diagnostic_list(kind);
            self.open_result_list(kind, None, window, cx);
        }
    }

    /// Close a diagnostic results window on the current page.
    pub(super) fn close_diagnostics(
        &mut self,
        current_buffer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(kind) = self.diagnostic_list_kind(current_buffer) {
            self.close_result_list(kind, window, cx);
        }
    }

    /// Toggle the diagnostics window without replacing either jump list.
    pub(super) fn toggle_diagnostics(
        &mut self,
        current_buffer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(kind) = self.diagnostic_list_kind(current_buffer) {
            if self.projects.active().result_pane(kind).is_some() {
                self.close_diagnostics(current_buffer, window, cx)
            } else {
                self.show_diagnostics(current_buffer, window, cx)
            }
        }
    }

    /// Synchronize visible live diagnostics after providers publish a new revision.
    pub(super) fn refresh_diagnostic_lists(&mut self) -> bool {
        let Some(project) = self.projects.active_project() else {
            return false;
        };
        let kinds: Vec<_> = project
            .page_editors()
            .filter_map(|(_, editor)| editor.buffer().list_owner().map(|(kind, _)| kind))
            .filter(|kind| {
                matches!(
                    kind,
                    ResultList::Diagnostics | ResultList::BufferDiagnostics(_)
                )
            })
            .collect();
        let mut changed = false;
        for kind in kinds {
            changed |= self.refresh_diagnostic_list(kind);
        }
        changed
    }

    /// Rebuild one live diagnostics view when its validated entries change.
    fn refresh_diagnostic_list(&mut self, kind: ResultList) -> bool {
        let buffer = match kind {
            ResultList::BufferDiagnostics(number) => Some(number),
            ResultList::Diagnostics => None,
            _ => return false,
        };
        let entries = self.diagnostic_entries(buffer);
        let history = self.projects.active_mut().result_lists.history(kind);
        if history.current().is_none() {
            history.push(List::default());
        }
        let list = history.current_mut().unwrap();
        if list.entries == entries {
            return false;
        }
        list.current = list.current.min(entries.len().saturating_sub(1));
        list.entries = entries;
        self.update_result_list_windows(kind, None);
        true
    }
}
