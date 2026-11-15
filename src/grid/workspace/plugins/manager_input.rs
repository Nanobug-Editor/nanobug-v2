//! Workspace adapter for manager input.

use super::*;

impl Workspace {
    pub(in crate::grid::workspace) fn open_plugin_manager(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugins.session.is_none() {
            return;
        }
        self.popup_backdrop.invalidate();
        self.plugins.open = true;
        self.clear_canvas_focus();
        self.end_plugin_field_drag();
        if let Some(trace) = &self.plugins.trace {
            trace.mark("manager_open", serde_json::Value::Null);
        }
        self.plugins.surface.focused = false;
        self.plugins.composition.clear();
        self.plugins.manager.all_projects = self.plugins.active.is_none();
        self.plugins.manager.local_project = false;
        if self.plugins.manager.page == Page::Marketplace {
            self.plugins.send(Action::OpenMarketplace);
        }
        self.plugin_refresh(window, cx);
    }

    /// Browse for a local archive in `window` from this profile's last ZIP directory.
    /// Restore the manager through `cx` when the chooser closes.
    pub(in crate::grid::workspace) fn choose_plugin_zip(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = self
            .projects
            .active_project()
            .map(|project| project.root.clone());
        let remembered_directory = self
            .plugins
            .session
            .as_ref()
            .map_or_else(|| Arc::clone(&self.plugins.view), Session::view)
            .local_zip_directory
            .clone();
        self.plugins.open = false;
        self.plugins.composition.clear();
        if !self.choose_file(
            initial,
            remembered_directory,
            &["zip"],
            |workspace, path, window, cx| {
                workspace.plugins.open = true;
                workspace.popup_backdrop.invalidate();
                if let Some(path) = path {
                    workspace.plugins.send(Action::InspectChosenZip(path));
                }
                workspace.plugin_refresh(window, cx);
            },
            window,
            cx,
        ) {
            self.plugins.open = true;
        }
    }

    /// Show document results in the native list with the cursor in command mode.
    pub(in crate::grid::workspace) fn open_document_list(&mut self) {
        self.plugins.open = true;
        self.plugins.manager.prompt = None;
        self.plugins.manager.help = false;
        self.plugins.manager.feedback = None;
        self.plugins.manager.selection = 0;
        self.plugins.manager.offset = 0;
        self.plugins.composition.clear();
    }

    pub(in crate::grid::workspace) fn plugin_refresh(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(trace) = &self.plugins.trace {
            trace.mark(
                "manager_input",
                serde_json::json!({
                    "open": self.plugins.open,
                    "prompt": self.plugins.manager.prompt.is_some(),
                    "page": self.plugins.manager.page as usize,
                    "details": self.plugins.manager.detail.is_some(),
                    "help": self.plugins.manager.help,
                    "search_bytes": self.plugins.manager.search.len(),
                    "selection": self.plugins.manager.selection,
                    "surface_input": self.plugins.surface.focused,
                }),
            );
        }
        self.sync_text_input(window, cx);
        self.update_cursor(window, cx);
        cx.notify();
    }

    pub(in crate::grid::workspace) fn advance_install_review(
        &mut self,
        token: u64,
        forward: bool,
        approve: bool,
    ) {
        let Some(review) = self
            .plugins
            .view
            .review
            .as_ref()
            .filter(|review| review.token == token)
        else {
            return;
        };
        let (pages, current) = self
            .plugin_manager_view()
            .install_review_page(review, self.plugin_manager_area());
        self.plugins.manager.review_token = Some(token);
        self.plugins.manager.review_page = if forward {
            (current + 1).min(pages.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        if approve && current + 1 == pages.len() {
            self.plugins.send(Action::Approve(token));
        }
    }

    /// Use native text entry only for the active prompt or plugin text field.
    pub(in crate::grid::workspace) fn plugin_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.plugins.file_busy
            || self.plugins.credential_waiting
            || self
                .plugins
                .close_draft
                .as_ref()
                .is_some_and(|close| close.waiting)
        {
            return true;
        }
        let key = &event.keystroke;
        let raw = key.key.as_str();
        if raw == "f6" && self.plugins.composition.is_empty() {
            if !event.is_held {
                self.cycle_cursor_effect(key.modifiers.shift, window, cx);
            }
            return true;
        }
        if self.plugins.accepts_text() {
            if raw == "escape" {
                if self.plugins.open {
                    self.finish_plugin_prompt(true, window, cx);
                } else {
                    self.plugins.surface.focused = false;
                    self.plugins.surface.key = None;
                    self.plugins.surface.viewport = None;
                    self.end_plugin_field_drag();
                }
                self.plugins.composition.clear();
            } else if raw == "enter" && self.plugins.composition.is_empty() {
                if self.plugins.open {
                    self.finish_plugin_prompt(false, window, cx);
                } else if let Some(key) = self.plugins.surface.key.clone()
                    && let Some(revision) = self
                        .current_plugin_surface()
                        .map(|surface| surface.revision)
                {
                    self.plugins.send(Action::SurfaceAction {
                        key,
                        revision,
                        action: "submit".into(),
                        argument: serde_json::Value::Null,
                    });
                }
            } else if !self.plugins.open
                && self.plugins.composition.is_empty()
                && let Some(changed) = self
                    .plugins
                    .surface
                    .field
                    .edit(&mut self.plugins.surface.draft, key)
            {
                if changed {
                    self.plugins.request_draft();
                }
            } else if raw == "backspace" && self.plugins.composition.is_empty() && self.plugins.open
            {
                if let Some(input) = &mut self.plugins.manager.prompt {
                    if let Prompt::Credential { value, .. } = &mut input.kind {
                        if let Some((start, _)) = value.grapheme_indices(true).next_back() {
                            let mut prefix = Zeroizing::new(String::with_capacity(2048));
                            prefix.push_str(&value[..start]);
                            *value = prefix;
                        }
                    } else {
                        pop_grapheme(&mut input.value);
                    }
                    self.plugins.manager.live_search();
                }
            } else if raw == "v" && (key.modifiers.control || key.modifiers.platform) {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.plugin_commit(&text, window, cx);
                }
            } else {
                return false;
            }
            self.plugin_refresh(window, cx);
            return true;
        }
        if !self.plugins.open {
            return false;
        }
        let command = normal_character(key);
        let printable = command.as_deref().unwrap_or_default();
        let completing = self
            .plugins
            .language
            .as_ref()
            .is_some_and(|results| results.is_completion());
        let completion_key = if completing && raw == "tab" && !key.modifiers.modified() {
            "enter"
        } else if completing
            && key.modifiers.control
            && !key.modifiers.shift
            && !key.modifiers.alt
            && !key.modifiers.platform
            && !key.modifiers.function
        {
            match raw {
                "n" => "down",
                "p" => "up",
                "y" => "enter",
                "e" => "escape",
                _ => raw,
            }
        } else {
            raw
        };
        if self.plugins.manager.help {
            if matches!(raw, "escape" | "q") || printable == "?" {
                self.plugins.manager.help = false;
            }
        } else if let Some(review) = &self.plugins.view.review {
            let token = review.token;
            match raw {
                "enter" if !event.is_held => self.advance_install_review(token, true, true),
                "right" if !event.is_held => self.advance_install_review(token, true, false),
                "left" if !event.is_held => self.advance_install_review(token, false, false),
                "escape" | "q" => {
                    self.plugins.send(Action::CancelInstall);
                }
                _ => {}
            }
        } else if let Some(namespace) = &self.plugins.manager.confirm_remove {
            if raw == "enter" && !event.is_held {
                self.plugins.send(Action::Uninstall(namespace.clone()));
                self.plugins.manager.confirm_remove = None;
            } else if matches!(raw, "escape" | "q") {
                self.plugins.manager.confirm_remove = None;
            }
        } else {
            let count = self
                .plugin_manager_view()
                .plugin_row_count(self.plugin_manager_area());
            match (completion_key, printable) {
                ("escape", _) if !event.is_held => self.plugin_back(),
                (_, "q") => self.plugin_back(),
                (_, "?") => self.plugins.manager.help = true,
                (_, "/")
                    if !event.is_held && self.plugin_manager_view().uses_plugin_list_layout() =>
                {
                    self.plugins.manager.open_search()
                }
                ("up", _) | (_, "k") => {
                    self.plugins.manager.selection =
                        self.plugins.manager.selection.saturating_sub(1)
                }
                ("down", _) | (_, "j") => {
                    self.plugins.manager.selection =
                        (self.plugins.manager.selection + 1).min(count.saturating_sub(1))
                }
                ("home", _) => self.plugins.manager.selection = 0,
                ("end", _) => self.plugins.manager.selection = count.saturating_sub(1),
                ("enter", _) if !event.is_held => self.plugin_activate(window, cx),
                (_, "1" | "2" | "3" | "4")
                    if self.plugins.outline.is_none() && self.plugins.language.is_none() =>
                {
                    self.plugin_tab(printable.as_bytes()[0] - b'1')
                }
                _ => {}
            }
        }
        self.plugin_refresh(window, cx);
        true
    }

    pub(in crate::grid::workspace) fn plugin_commit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.plugins.accepts_text() {
            return false;
        }
        if let Some(input) = &mut self.plugins.manager.prompt {
            let multiline = matches!(input.kind, Prompt::Rule { .. } | Prompt::Setting { .. });
            let clean = || {
                text.chars()
                    .filter(|&ch| !ch.is_control() || multiline && matches!(ch, '\t' | '\n'))
            };
            let (value, limit) = match &mut input.kind {
                Prompt::Credential { value, .. } => (&mut **value, 2048),
                _ => (&mut input.value, 64 * 1024),
            };
            if value.len() + clean().map(char::len_utf8).sum::<usize>() <= limit {
                value.extend(clean());
            } else {
                self.plugins.manager.feedback =
                    Some("Input is too long. The text was kept unchanged.".into());
            }
            self.plugins.manager.live_search();
        } else {
            let clean = text
                .chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>();
            if self
                .plugins
                .surface
                .field
                .insert(&mut self.plugins.surface.draft, &clean)
            {
                self.plugins.request_draft();
            } else {
                self.set_status("Plugin input is too long. The draft was kept unchanged.".into());
            }
        }
        self.plugin_refresh(window, cx);
        true
    }

    pub(in crate::grid::workspace) fn plugin_click(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (x, y) = self.metrics.cell(event.position);
        if self.plugins.open {
            if self.plugins.file_busy
                || self.plugins.credential_waiting
                || self
                    .plugins
                    .close_draft
                    .as_ref()
                    .is_some_and(|close| close.waiting)
            {
                return true;
            }
            let area = self.plugin_manager_area();
            if area.contains(x, y) && event.button == gpui::MouseButton::Left {
                let x = x - area.x;
                let row = y - area.y;
                self.plugin_manager_click(area, (x, row), event.click_count > 1, window, cx);
            }
            self.plugin_refresh(window, cx);
            return true;
        }
        if let Some(area) = self.plugin_surface_area()
            && area.contains(x, y)
        {
            if self
                .plugin_input_bounds(area)
                .is_some_and(|input| input.contains(&event.position))
            {
                self.clear_canvas_focus();
                self.plugins.surface.focused = true;
                let input = self.plugin_surface_input_area(area);
                self.press_plugin_field(input, event.position, event.modifiers.shift);
            } else if !self.plugin_canvas_down(event)
                && let Some(surface) = self.canvas_surface()
            {
                let bounds = area.pixels(self.metrics);
                let position = event.position - bounds.origin;
                let (px, py) = (f32::from(position.x), f32::from(position.y));
                if let Some(hit) = surface.frame.canvas.as_ref().and_then(|canvas| {
                    canvas.hits.iter().rev().find(|hit| {
                        let [x, y, w, h] = hit.bounds;
                        (x..x + w).contains(&px) && (y..y + h).contains(&py)
                    })
                }) {
                    let key = self.plugins.surface.key.clone().expect("Canvas owner");
                    self.plugins.send(Action::SurfaceAction {
                        key,
                        revision: surface.revision,
                        action: hit.action.clone(),
                        argument: serde_json::json!({"x": px, "y": py}),
                    });
                }
            }
            self.plugin_refresh(window, cx);
            return true;
        }
        self.clear_canvas_focus();
        self.end_plugin_field_drag();
        if self.plugins.surface.focused {
            self.plugins.surface.focused = false;
            self.plugins.composition.clear();
            self.plugin_refresh(window, cx);
        }
        false
    }

    /// Dispatch a panel-relative click. A prompt or confirmation owns the mouse until it closes.
    pub(in crate::grid::workspace) fn plugin_manager_click(
        &mut self,
        area: Rect,
        (x, row): (usize, usize),
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugins.manager.help {
            return;
        }
        let review = self.plugins.view.review.as_ref().map(|review| review.token);
        let removal = self.plugins.manager.confirm_remove.as_ref();
        if review.is_some() || removal.is_some() {
            if row == area.h - 2
                && let Some(cancel) = prompt_button_at(area.w.saturating_sub(SEARCH_BUTTONS + 2), x)
            {
                if let Some(token) = review {
                    if cancel {
                        self.plugins.send(Action::CancelInstall);
                    } else {
                        self.advance_install_review(token, true, true);
                    }
                } else if let Some(namespace) = self.plugins.manager.confirm_remove.take()
                    && !cancel
                {
                    self.plugins.send(Action::Uninstall(namespace));
                }
            }
            return;
        }

        let layout = self
            .plugin_manager_view()
            .uses_plugin_list_layout()
            .then(|| self.plugin_manager_view().plugin_list_layout(area));
        if let Some(input) = &self.plugins.manager.prompt {
            let cancel = match &layout {
                Some(layout) if layout.footer.contains(x, row) => {
                    layout.prompt(&input.kind, &self.spacing).cancel_at(x)
                }
                None if row == 1 => prompt_button_at(area.w.saturating_sub(SEARCH_BUTTONS + 2), x),
                _ => None,
            };
            if let Some(cancel) = cancel {
                self.finish_plugin_prompt(cancel, window, cx);
            }
            return;
        }
        let entries = if let Some(layout) = layout {
            if row == layout.list.y {
                if let Some(tab) = layout
                    .tabs(&self.spacing)
                    .iter()
                    .position(|slot| slot.contains(&x))
                {
                    self.plugin_tab(tab as u8);
                }
                return;
            }
            layout.entries
        } else {
            secondary_list_entries(area)
        };
        if entries.contains(x, row) {
            let visible = self
                .plugin_manager_view()
                .plugin_visible_rows(entries.h, self.plugin_manager_view().plugin_row_count(area));
            let index = visible.start + row - entries.y;
            if visible.contains(&index) {
                self.plugins.manager.selection = index;
                if activate {
                    self.plugin_activate(window, cx);
                }
            }
        }
    }

    pub(in crate::grid::workspace) fn plugin_tab(&mut self, index: u8) {
        if self.plugins.credential_draft.is_none()
            && let Some(action) = self.plugins.manager.select_tab(index)
        {
            self.plugins.send(action);
        }
    }

    pub(in crate::grid::workspace) fn plugin_back(&mut self) {
        if self.plugins.credential_draft.is_some() {
            self.edit_plugin_credential();
            return;
        }
        if self.plugins.formatting.choices.take().is_some() {
            self.plugins.open = false;
            return;
        }
        if self.plugins.file_plan.take().is_some() || !self.plugins.file_report.is_empty() {
            self.plugins.file_report.clear();
            return;
        }
        if self.plugins.language.take().is_some() || self.plugins.outline.take().is_some() {
            self.plugins.open = false;
            return;
        }
        if self.plugins.edit_plan.take().is_some() || self.plugins.edit_check.take().is_some() {
            return;
        }
        if self.plugins.close_draft.take().is_some() {
            self.plugins.open = false;
            return;
        }
        let manager = &mut self.plugins.manager;
        if self.plugins.view.installing {
            self.plugins.send(Action::CancelInstall);
            return;
        }
        if matches!(manager.detail_page, Detail::Settings | Detail::Cleanup) {
            manager.detail_page = Detail::Overview;
            manager.selection = 0;
        } else if manager.detail_page == Detail::Compare {
            if let Some(comparison) = &mut manager.comparison
                && let Some(preview) = comparison.preview.take()
            {
                (manager.selection, manager.offset) = preview.from;
            } else if let Some(comparison) = manager.comparison.take() {
                (manager.detail_page, manager.selection, manager.offset) = comparison.from;
            } else {
                manager.detail_page = Detail::Rules;
                manager.selection = 0;
            }
        } else if manager.detail_page == Detail::Operations {
            manager.detail_page = Detail::Overview;
            manager.selection = 0;
        } else if manager.detail.take().is_some() {
            manager.selection = manager.list_selection;
            manager.offset = 0;
            manager.detail_page = Detail::Overview;
        } else {
            self.plugins.open = false;
        }
    }

    pub(in crate::grid::workspace) fn plugin_activate(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_manager_view().comparison_preview().is_some() {
            return;
        }
        let rows = self.plugins.rows();
        let selected = self
            .plugins
            .manager
            .selection
            .min(rows.len().saturating_sub(1));
        let Some(row) = rows.into_iter().nth(selected) else {
            return;
        };
        match row.target {
            Target::None => {}
            Target::Details(namespace) => {
                self.plugins.manager.detail = Some(namespace);
                self.plugins.manager.list_selection = selected;
                self.plugins.manager.selection = 0;
                self.plugins.manager.offset = 0;
            }
            Target::Zip => self.choose_plugin_zip(window, cx),
            Target::Development => {
                self.plugins.manager.prompt = Some(Input {
                    kind: Prompt::Development,
                    value: String::new(),
                });
            }
            Target::Action(action) => {
                if let Action::ResolveConfig(scope, revision, _) = action {
                    if self
                        .plugins
                        .view
                        .configurations
                        .get(&scope)
                        .is_none_or(|view| view.revision != revision)
                    {
                        self.plugin_back();
                        self.plugins.manager.feedback =
                            Some(i18n::text("plugin_manager.compare_changed_again").into());
                        return;
                    }
                    if self.plugins.send(action) {
                        self.plugin_back();
                    }
                } else {
                    self.plugins.send(action);
                }
            }
            Target::Uninstall(key) => self.plugins.manager.confirm_remove = Some(key),
            Target::Surface(key) => {
                let Some(surface) = self.plugins.view.surfaces.get(&key) else {
                    return;
                };
                self.plugins.open = false;
                self.plugins.surface.focused = surface.frame.input.is_some()
                    || surface
                        .frame
                        .canvas
                        .as_ref()
                        .is_some_and(|canvas| canvas.has_input());
                self.plugins.surface.draft.clone_from(&surface.draft);
                self.plugins.surface.field = field::Field::default();
                self.plugins.surface.key = Some(key);
                self.plugins.surface.pending_draft = None;
                self.plugins.surface.viewport = None;
            }
            Target::Scope => {
                if self.plugins.active.is_some() {
                    let manager = &mut self.plugins.manager;
                    if manager.detail_page == Detail::Settings
                        && !manager.all_projects
                        && !manager.local_project
                    {
                        manager.local_project = true;
                    } else {
                        manager.all_projects = !manager.all_projects;
                        manager.local_project = false;
                    }
                }
            }
            Target::Detail(detail) => {
                if detail == Detail::Compare {
                    let configurations = Arc::clone(&self.plugins.view.configurations);
                    let Some(scope) = self
                        .plugins
                        .scope()
                        .filter(|scope| configurations.contains_key(scope))
                    else {
                        return;
                    };
                    let manager = &mut self.plugins.manager;
                    manager.comparison = Some(comparison::Comparison {
                        scope,
                        configurations,
                        from: (manager.detail_page, selected, manager.offset),
                        preview: None,
                    });
                }
                self.plugins.manager.detail_page = detail;
                self.plugins.manager.selection = 0;
                self.plugins.manager.offset = 0;
                if detail == Detail::Operations
                    && let Some(namespace) = &self.plugins.manager.detail
                {
                    self.plugins
                        .send(Action::ReviewToolOperations(namespace.clone()));
                }
                if detail == Detail::Cleanup {
                    self.plugins.manager.clean_credentials = false;
                    self.plugins.manager.clean_local = true;
                }
            }
            Target::Compare(source) => {
                let manager = &mut self.plugins.manager;
                if let Some(comparison) = &mut manager.comparison
                    && comparison.open(source, selected, manager.offset)
                {
                    manager.selection = 0;
                    manager.offset = 0;
                }
            }
            Target::Retained => {
                self.plugins.manager.retained_open = !self.plugins.manager.retained_open
            }
            Target::CleanCredentials => {
                self.plugins.manager.clean_credentials = !self.plugins.manager.clean_credentials
            }
            Target::CleanLocal => {
                self.plugins.manager.clean_local = !self.plugins.manager.clean_local
            }
            Target::Prompt(input) => self.plugins.manager.prompt = Some(input),
            Target::RetryDraftClose => self.advance_plugin_close(window, cx),
            Target::ExportDraft => {
                self.plugins.manager.prompt = Some(Input {
                    kind: Prompt::ExportDraft,
                    value: String::new(),
                })
            }
            Target::DiscardDraftClose => {
                let scope = self
                    .plugins
                    .close_draft
                    .as_ref()
                    .and_then(|close| close.project);
                let (reply, receive) = async_channel::bounded(1);
                self.wait_draft_close(Action::DiscardDrafts(scope, reply), receive, window, cx);
            }
            Target::CancelDraftClose => self.plugin_back(),
            Target::ApplyCredential => self.save_plugin_credential(window, cx),
            Target::EditCredential => self.edit_plugin_credential(),
            Target::DiscardCredential => {
                self.plugins.credential_draft = None;
                self.finish_credential_close(window, cx);
            }
            Target::Command(key, id) => self.plugin_command(key, id, window, cx),
            Target::ApplyEdits => self.apply_plugin_edits(window, cx),
            Target::ApplyFiles => self.apply_plugin_files(window, cx),
            Target::Formatter(key) => self.choose_formatter(key, window, cx),
            Target::Completion(text) => {
                if let Some(edit) = self
                    .plugins
                    .language
                    .as_ref()
                    .map(|results| results.insertion(text))
                {
                    self.use_language_result(Some(edit), None, window, cx);
                }
            }
            Target::CompletionEdit(edit) => self.use_language_result(Some(edit), None, window, cx),
            Target::Diagnostic(offset) => self.use_language_result(None, Some(offset), window, cx),
            Target::Definition(item) => self.use_definition(item, window, cx),
            Target::Symbol(offset) => self.jump_to_symbol(offset, window, cx),
        }
    }
}
