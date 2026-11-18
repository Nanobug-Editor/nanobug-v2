//! Full-file preview, read-only input, and shortcut captions for the picker.

use super::{Operation, PickerSession, Workspace};
use crate::{
    grid::{
        FilePreviewProvider, PluginPreview, PreviewHighlighter, PreviewShortcut,
        render::{
            Frame, GridLayer, LabelStyle, MessageLevel, Metrics, Panel, SHORTCUT_SEPARATOR,
            shortcut_hint,
        },
        windows::{
            WindowFrame,
            editor::EditorWindow,
            file_picker::{PickerLayout, Preview, SyntaxColors},
        },
        workspace::viewport::CursorSpace,
    },
    ui::{
        keys::normal_character,
        text::{TextSpacing, UNITS_PER_COLUMN},
        theme::{MUTED, TEXT, ThemeComponent},
    },
};
use gpui::{AppContext, ClipboardItem, Context, Entity, KeyDownEvent, Modifiers, Task, Window};
use nanobug::{
    animation::scroll::ScrollInput,
    document::Document,
    i18n,
    vim::{Action, Mode},
};
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

/// One file entered from the directory list. Dropping it cancels background delivery.
pub(super) struct FocusedFile {
    /// Absolute file path used for preview, provider selection and external opening.
    pub(super) path: PathBuf,
    content: FileContent,
    load: Option<Task<()>>,
}

/// Loaded UI state for a full-file preview.
enum FileContent {
    Loading,
    Text {
        editor: Box<EditorWindow>,
        colors: SyntaxColors,
        numbers: Entity<Panel>,
    },
    Plugin(Box<dyn PluginPreview>),
    Unavailable(Option<String>),
}

/// Preview data delivered from the background worker without window entities.
enum LoadedFile {
    Text(Box<Document>, SyntaxColors),
    Plugin(Box<dyn PluginPreview>),
    Unavailable(Option<String>),
}

impl LoadedFile {
    /// Read `path` as editor text, then ask the optional `provider` about other file types.
    /// `highlighter` colors only decoded text.
    fn read(
        path: &Path,
        provider: Option<FilePreviewProvider>,
        highlighter: Option<PreviewHighlighter>,
    ) -> Self {
        let document = (!Preview::requires_system_open(path)).then(|| {
            std::fs::metadata(path)?;
            Document::open(path)
        });
        if let Some(Ok(doc)) = document {
            let colors = highlighter.map_or_else(SyntaxColors::default, |service| {
                SyntaxColors::new(&doc.text, (service.0)(path, &doc.text))
            });
            return Self::Text(Box::new(doc), colors);
        }
        if let Some(provider) = provider {
            match (provider.0)(path) {
                Ok(Some(preview)) => return Self::Plugin(preview),
                Ok(None) => {}
                Err(error) => return Self::Unavailable(Some(format!("{error:#}"))),
            }
        }
        Self::Unavailable(
            document
                .and_then(Result::err)
                .map(|error| format!("{error:#}")),
        )
    }
}

impl FocusedFile {
    /// Borrow the editor once text has loaded.
    pub(super) fn editor(&self) -> Option<&EditorWindow> {
        match &self.content {
            FileContent::Text { editor, .. } => Some(editor),
            _ => None,
        }
    }

    /// Borrow the active editor for input or viewport movement.
    pub(super) fn editor_mut(&mut self) -> Option<&mut EditorWindow> {
        match &mut self.content {
            FileContent::Text { editor, .. } => Some(editor),
            _ => None,
        }
    }

    /// Return the placeholder for an unavailable or pending preview.
    pub(super) fn status(&self) -> Option<&str> {
        match &self.content {
            FileContent::Loading => Some(i18n::text("picker.loading")),
            FileContent::Unavailable(reason) => Some(
                reason
                    .as_deref()
                    .unwrap_or_else(|| i18n::text("picker.unavailable")),
            ),
            _ => None,
        }
    }

    /// Draw the preview in `layout` with the current focus and optional syntax colors.
    pub(super) fn refresh(
        &self,
        layout: PickerLayout,
        active: bool,
        spacing: &TextSpacing,
        text_panel: &Entity<Panel>,
        cx: &mut Context<Workspace>,
    ) {
        let area = layout.preview_editor_area();
        let frame = match &self.content {
            FileContent::Text {
                editor,
                colors,
                numbers,
            } => {
                let context = WindowFrame {
                    area,
                    selected: active,
                    modal_open: !active,
                    controls_hovered: false,
                    spacing,
                };
                let (gutter, frame) = editor.content_frames(context, |byte| colors.at(byte));
                numbers.update(cx, |panel, cx| panel.set(gutter, cx));
                frame
            }
            FileContent::Plugin(preview) => {
                let body = EditorWindow::body(area);
                preview.frame(body.w, body.h, spacing)
            }
            _ => return,
        };
        text_panel.update(cx, |panel, cx| panel.set(frame, cx));
    }

    /// Append the focused content layers to `layers`, clipping them to the preview pane.
    pub(super) fn layers(
        &self,
        layout: PickerLayout,
        metrics: Metrics,
        text_panel: &Entity<Panel>,
        layers: &mut Vec<GridLayer>,
    ) {
        let area = layout.preview_editor_area();
        match &self.content {
            FileContent::Text {
                editor, numbers, ..
            } => {
                layers.extend(editor.content_layers(area, metrics, numbers, text_panel));
            }
            FileContent::Plugin(_) => {
                let bounds = EditorWindow::body(area).pixels(metrics);
                layers.push(GridLayer {
                    panel: text_panel.clone(),
                    bounds,
                    clip: Some(bounds),
                });
            }
            _ => {}
        }
    }
}

/// Return the area in `width` columns reserved for preview command input.
fn command_slot(width: usize, spacing: &TextSpacing) -> (Range<usize>, String) {
    let apply = shortcut_hint(i18n::text("picker.hint_apply"), "Enter");
    let cancel = shortcut_hint(i18n::text("picker.hint_cancel"), "Esc");
    let hints = format!("{apply}{SHORTCUT_SEPARATOR}{cancel}");
    let end = width.saturating_sub(1);
    let right = spacing.columns(&hints);
    (1..end.saturating_sub(right + 2).max(1), hints)
}

impl PickerSession {
    /// Generate a footer of complete shortcuts for the current picker input mode.
    pub(super) fn footer_frame(&self, width: usize, spacing: &TextSpacing) -> Frame {
        let mut frame = Frame::new(width, 1);
        frame.set_theme_component(ThemeComponent::Footer);
        let mut column = 1;
        let mut push = |action: &str, key: &str| {
            let label = shortcut_hint(action, key);
            let separator = if column > 1 { SHORTCUT_SEPARATOR } else { "" };
            let end = column + separator.len() + spacing.columns(&label);
            if end >= width {
                return false;
            }
            column = frame.styled_label(
                0,
                column..end,
                separator,
                MUTED,
                spacing,
                LabelStyle::Dimmed,
            );
            column = frame.styled_label(0, column..end, &label, TEXT, spacing, LabelStyle::Dimmed);
            true
        };
        let shortcuts: &[(&str, &str)] = if let Some(file) = &self.focused {
            match &file.content {
                FileContent::Text { editor, .. }
                    if matches!(editor.vim().mode, Mode::Command | Mode::Search) =>
                {
                    let (slot, hints) = command_slot(width, spacing);
                    let hints_start = slot.end + 2;
                    let command = spacing
                        .ellipsize(&editor.command_text(), slot.len())
                        .into_owned();
                    frame.label(0, slot, &command, TEXT, spacing);
                    frame.styled_label(
                        0,
                        hints_start..width.saturating_sub(1),
                        &hints,
                        MUTED,
                        spacing,
                        LabelStyle::Dimmed,
                    );
                    return frame;
                }
                FileContent::Text { editor, .. } => {
                    let pending = editor.vim().mode.is_visual() || editor.vim().awaiting_key();
                    let escape = if pending {
                        "picker.hint_normal"
                    } else {
                        "picker.hint_back"
                    };
                    push(i18n::text(escape), "Esc");
                    if !pending {
                        push(i18n::text("picker.hint_help"), "Space ?");
                    }
                    &[
                        ("picker.hint_move", "h/j/k/l"),
                        ("picker.hint_find", "/ ? n/N"),
                        ("picker.hint_select", "v/V/Ctrl+v"),
                        ("picker.hint_copy", "y"),
                        ("picker.hint_page", "Ctrl+d/u"),
                    ]
                }
                FileContent::Plugin(preview) => {
                    push(i18n::text("picker.hint_back"), "Esc");
                    let actions: &[PreviewShortcut] = preview.shortcuts();
                    for action in actions {
                        if !push(&action.action, &action.key) {
                            break;
                        }
                    }
                    return frame;
                }
                FileContent::Unavailable(_) => {
                    &[("picker.hint_back", "Esc"), ("picker.hint_external", "O")]
                }
                FileContent::Loading => &[("picker.hint_back", "Esc")],
            }
        } else if self.state.prompt.is_some() || self.state.delete.is_some() {
            &[
                ("picker.hint_apply", "Enter"),
                ("picker.hint_cancel", "Esc"),
            ]
        } else if self.state.goto {
            &[
                ("picker.hint_jump", "gg/gh/gd/g Space"),
                ("picker.hint_exit", "Esc"),
            ]
        } else if self.state.busy {
            if self.operation.is_none() {
                &[("picker.hint_exit", "Esc")]
            } else {
                &[]
            }
        } else {
            let escape = if !self.state.search.is_empty() {
                "picker.hint_clear_search"
            } else if !self.state.filter.is_empty() {
                "picker.hint_clear_filter"
            } else {
                "picker.hint_exit"
            };
            push(i18n::text(escape), "Esc");
            if !self.state.search.is_empty() {
                push(i18n::text("picker.hint_jump"), "n/N");
            }
            let [first, second] = if self.choosing_file() {
                let activate = if self.state.entry().is_some_and(|entry| entry.directory) {
                    "picker.hint_enter"
                } else {
                    "picker.hint_select"
                };
                [(activate, "Enter"), ("picker.hint_browse", "h/l")]
            } else {
                [("picker.hint_enter", "l"), ("picker.hint_open", "Enter")]
            };
            &[
                ("picker.hint_move", "j/k"),
                first,
                second,
                ("picker.hint_help", "Space ?"),
                ("picker.hint_find", "/"),
                ("picker.hint_filter", "f"),
                ("picker.hint_mark", "s"),
            ]
        };
        for &(name, keys) in shortcuts {
            if !push(i18n::text(name), keys) {
                break;
            }
        }
        frame
    }
}

impl Workspace {
    /// Enter a selected `path` from the directory list and load its content for `window` through `cx`.
    pub(super) fn enter_file_preview(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let provider = cx.try_global::<FilePreviewProvider>().cloned();
        let highlighter = cx.try_global::<PreviewHighlighter>().cloned();
        let requested = path.clone();
        let read = cx
            .background_executor()
            .spawn(async move { LoadedFile::read(&requested, provider, highlighter) });
        let task_path = path.clone();
        let load = cx.spawn_in(window, async move |view, cx| {
            let loaded = read.await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |workspace, cx| {
                    let Some(file) = workspace
                        .file_picker
                        .as_mut()
                        .and_then(|session| session.focused.as_mut())
                        .filter(|file| file.path == task_path)
                    else {
                        return;
                    };
                    file.content = match loaded {
                        LoadedFile::Text(doc, colors) => FileContent::Text {
                            editor: Box::new(EditorWindow::read_only(*doc)),
                            colors,
                            numbers: cx.new(|_| Panel::new(workspace.metrics)),
                        },
                        LoadedFile::Plugin(preview) => FileContent::Plugin(preview),
                        LoadedFile::Unavailable(reason) => FileContent::Unavailable(reason),
                    };
                    file.load = None;
                    workspace.sync(window, cx);
                });
            });
        });
        let session = self.file_picker.as_mut().expect("active picker");
        session.focused = Some(FocusedFile {
            path,
            content: FileContent::Loading,
            load: Some(load),
        });
        session.message = None;
        self.last_cursor_space = None;
        self.sync(window, cx);
    }

    /// Restore directory browsing after an unmodified Escape or Vim close request.
    fn leave_file_preview(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.file_picker.as_mut().expect("active picker").focused = None;
        self.held_close_key = Some(key.into());
        self.last_cursor_space = None;
        self.sync(window, cx);
    }

    /// Give the provider `event` before starting a host shortcut, except for the return key.
    pub(in crate::grid::workspace) fn plugin_preview_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = &event.keystroke;
        if self.prefix.is_some() || key.key == "escape" && key.modifiers == Modifiers::default() {
            return false;
        }
        let Some(FocusedFile {
            content: FileContent::Plugin(preview),
            ..
        }) = self
            .file_picker
            .as_mut()
            .and_then(|session| session.focused.as_mut())
        else {
            return false;
        };
        if !preview.key(key) {
            return false;
        }
        self.sync(window, cx);
        true
    }

    /// Handle a native key in the focused preview, leaving text entry to native IME.
    pub(super) fn focused_file_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let area = self.file_picker_layout().preview_editor_area();
        let session = self.file_picker.as_mut().expect("active picker");
        if session.operation.is_some() {
            return true;
        }
        let file = session.focused.as_mut().expect("active preview");
        let key = &event.keystroke;
        let normal = file
            .editor()
            .is_none_or(|editor| editor.vim().mode == Mode::Normal && !editor.vim().awaiting_key());
        if normal && key.key == "escape" && key.modifiers == Modifiers::default() {
            if !event.is_held {
                self.leave_file_preview("escape", window, cx);
            }
            return true;
        }
        let FileContent::Text { editor, .. } = &mut file.content else {
            match &mut file.content {
                FileContent::Unavailable(_)
                    if normal_character(key).as_deref() == Some("O") && !event.is_held =>
                {
                    let path = file.path.clone();
                    self.run_picker_operation(Operation::External(path), window, cx);
                }
                _ => {}
            }
            return true;
        };
        if editor.vim().accepts_text()
            && (key.modifiers.control || key.modifiers.platform)
            && !key.modifiers.alt
            && !key.modifiers.function
            && key.key == "v"
        {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.file_picker_commit(&text, false, window, cx);
            }
            return true;
        }
        if !editor.composition().is_empty()
            && !(cfg!(windows)
                && editor.vim().awaits_literal_argument()
                && key.key == "escape"
                && !key.modifiers.modified())
        {
            return false;
        }
        let before = editor.register().yank_revision();
        let directory = file.path.parent().unwrap_or_else(|| Path::new("."));
        let Some(result) = editor.key(key, area, &self.spacing, directory, false) else {
            return false;
        };
        if editor.register().yank_revision() != before {
            cx.write_to_clipboard(ClipboardItem::new_string(editor.register().text.clone()));
        }
        self.finish_focused_file_action(
            result.edit.action,
            &key.key,
            [
                result
                    .view_top
                    .map_or(ScrollInput::Reveal, ScrollInput::PositionWithTail),
                result
                    .view_left
                    .map_or(ScrollInput::Reveal, ScrollInput::PositionWithTail),
            ],
            window,
            cx,
        );
        true
    }

    /// Dispatch an `action` from a focused preview, including one returned by asynchronous replay.
    pub(in crate::grid::workspace) fn finish_focused_file_action(
        &mut self,
        action: Action,
        close_key: &str,
        [rows, columns]: [ScrollInput; 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let warning = match action {
            Action::Close(_) | Action::QuitAll(_) => {
                self.leave_file_preview(close_key, window, cx);
                return;
            }
            Action::Open(name) | Action::OpenFile(name, None) => {
                if let Some(file) = self
                    .file_picker
                    .as_ref()
                    .and_then(|session| session.focused.as_ref())
                {
                    let path = file
                        .path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(name);
                    self.open_picker_path(&path, window, cx);
                }
                return;
            }
            Action::None => self
                .file_picker
                .as_ref()
                .and_then(PickerSession::focused_editor)
                .is_some_and(|editor| {
                    editor.vim().message == i18n::text("editor.read_only_preview")
                }),
            Action::SyntaxSelection => {
                self.request_syntax_selection(true, window, cx);
                return;
            }
            Action::Sleep(seconds) => {
                self.sleep_editor(seconds, window, cx);
                return;
            }
            Action::WindowExtras(nanobug::vim::WindowCommand::Sleep {
                duration,
                hide_caret,
            }) => {
                self.sleep_editor_for(duration, hide_caret, window, cx);
                return;
            }
            Action::CommandWindow(_)
            | Action::Help(_)
            | Action::HelpTags { .. }
            | Action::CheckHealth(_)
            | Action::QuitWithCode(_)
            | Action::Restart { .. }
            | Action::VimMenu { .. }
            | Action::NumberedMarks { .. }
            | Action::VimInfo { .. }
            | Action::TerminalOpen(_)
            | Action::Save(_)
            | Action::WriteFile { .. }
            | Action::SaveAs { .. }
            | Action::ReadFile { .. }
            | Action::WriteAll
            | Action::WriteQuitAll
            | Action::SaveVimConfig
            | Action::ListBuffers
            | Action::SelectBuffer(_)
            | Action::NavigateBuffer { .. }
            | Action::DeleteBuffer { .. }
            | Action::WindowCommand { .. }
            | Action::OpenFile(_, Some(_))
            | Action::JumpHistory { .. }
            | Action::ShowListing(_)
            | Action::ShowStyledListing(_)
            | Action::RedirectFile { .. }
            | Action::RedirectClipboard { .. }
            | Action::EditableListing(_)
            | Action::IncludeSplit { .. }
            | Action::Files(_)
            | Action::BufferLifecycle(_)
            | Action::SessionExport(_)
            | Action::Recovery(_)
            | Action::Sign(_)
            | Action::Filetype(_)
            | Action::LanguageServer { .. }
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
            | Action::DeleteNumberedMarks(_)
            | Action::FinishSource
            | Action::ListScripts
            | Action::EditSourcedScript { .. }
            | Action::Source { .. }
            | Action::Runtime { .. }
            | Action::RunBatch { .. }
            | Action::UndoBatch
            | Action::AlternateBuffer(_)
            | Action::OpenFileMark { .. }
            | Action::OpenExternal(_)
            | Action::TabNavigate(_, _)
            | Action::TabEndpoint(_)
            | Action::TabMove(_)
            | Action::ListTabs
            | Action::TabNew(_)
            | Action::TabClose
            | Action::TabOnly
            | Action::Only
            | Action::EmptyBuffer
            | Action::EmptySplit(_)
            | Action::CloseWindow(_)
            | Action::Arguments { .. }
            | Action::PreviewOpen(_)
            | Action::PreviewClose
            | Action::Split { .. }
            | Action::SaveClose
            | Action::BracketBuffer { .. }
            | Action::BracketDiagnostic { .. }
            | Action::Filter { .. }
            | Action::Format(_)
            | Action::SqlDialect { .. }
            | Action::CancelFormat
            | Action::UndoPlugin
            | Action::PluginComplete
            | Action::InsertCandidates { .. }
            | Action::PluginDiagnose
            | Action::PluginDefinition
            | Action::PluginHover
            | Action::PluginOutline
            | Action::PluginFold
            | Action::PluginCommand(_)
            | Action::Appearance(_, _) => true,
        };
        if warning {
            self.show_picker_message(
                MessageLevel::Warning,
                i18n::text("editor.read_only_preview").into(),
                window,
                cx,
            );
        }
        self.repaint([ScrollInput::Advance, rows, columns], window, cx);
    }

    /// Scroll the read-only editor with the same inputs and caret animation as the main pane.
    pub(in crate::grid::workspace) fn advance_file_preview(
        &mut self,
        inputs: [ScrollInput; 2],
        cx: &mut Context<Self>,
    ) -> bool {
        let area = self.file_picker_layout().preview_editor_area();
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let Some(editor) = self
            .file_picker
            .as_mut()
            .and_then(PickerSession::focused_editor_mut)
        else {
            return false;
        };
        let (changed, rows, columns) = editor.track_view(area, true, &self.spacing, inputs, now);
        if self.last_cursor_space == Some(CursorSpace::FilePreviewText) {
            let editor = self
                .file_picker
                .as_ref()
                .and_then(PickerSession::focused_editor)
                .expect("preview editor");
            let buffer = editor.buffer();
            self.follow_cursor_scroll(&buffer.scroll, rows, Some((&buffer.columns, columns)), cx);
        }
        changed
    }

    /// Resolve the preview's document or command caret within the picker.
    pub(in crate::grid::workspace) fn file_preview_caret(
        &self,
        editor: &EditorWindow,
    ) -> (f32, f32, f32, bool, bool) {
        if matches!(editor.vim().mode, Mode::Command | Mode::Search) {
            let (slot, _) = command_slot(self.layout.cols, &self.spacing);
            let units = self
                .spacing
                .fitting_cells(&editor.command_caret_text(), slot.len() * UNITS_PER_COLUMN)
                .last()
                .map_or(0, |cell| cell.end);
            let row =
                f32::from(self.footer_bounds().origin.y - self.metrics.origin.y) / self.metrics.lh;
            return (
                slot.start as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                row,
                1.,
                true,
                true,
            );
        }
        editor.body_caret(
            self.file_picker_layout().preview_editor_area(),
            &self.spacing,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FontId, Keystroke, NoopTextSystem};
    use std::sync::Arc;

    /// A provider with one action and a changing grid frame.
    struct Pages {
        page: usize,
        actions: [PreviewShortcut; 1],
    }

    impl PluginPreview for Pages {
        fn frame(&self, columns: usize, rows: usize, _: &TextSpacing) -> Frame {
            let mut frame = Frame::new(columns, rows);
            frame.text(0, 0, &self.page.to_string(), TEXT, columns);
            frame
        }

        fn shortcuts(&self) -> &[PreviewShortcut] {
            &self.actions
        }

        fn key(&mut self, key: &Keystroke) -> bool {
            if key.key != "space" {
                return false;
            }
            self.page += 1;
            true
        }
    }

    /// A binary file delegates both its rendering and its operation to the registered provider.
    #[test]
    fn binary_preview_uses_the_providers_action_and_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.glb");
        std::fs::write(&path, b"glTF\0").unwrap();
        let provider = FilePreviewProvider(Arc::new(|path| {
            assert_eq!(path.extension().unwrap(), "glb");
            Ok(Some(Box::new(Pages {
                page: 1,
                actions: [PreviewShortcut {
                    action: "Next page".into(),
                    key: "Space".into(),
                }],
            })))
        }));
        let LoadedFile::Plugin(mut preview) = LoadedFile::read(&path, Some(provider), None) else {
            panic!("expected provider preview");
        };
        assert_eq!(preview.shortcuts()[0].key, "Space");
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let before = preview.frame(20, 5, &spacing);
        assert!(preview.key(&Keystroke::parse("space").unwrap()));
        assert!(before != preview.frame(20, 5, &spacing));
        assert_eq!(std::fs::read(&path).unwrap(), b"glTF\0");
        assert!(matches!(
            LoadedFile::read(&path, None, None),
            LoadedFile::Unavailable(None)
        ));
    }
}
