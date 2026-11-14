//! Native command line and typed requests for file, window or shell operations.

use super::{Mode, Register, Vim};
use crate::i18n;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Effects requested by modal editing and executed by the host.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// No external action.
    None,
    /// Save, optionally to a new project-relative filename.
    Save(Option<String>),
    /// Open a file or project directory, relative to the current project when needed.
    Open(String),
    /// Open a file and jump to its one-based line.
    OpenAt(String, usize),
    /// Navigate listed editor buffers with a direction, endpoint rule and optional count.
    BracketBuffer {
        /// Move forward or select the last endpoint.
        forward: bool,
        /// Without a count, select the requested endpoint.
        edge: bool,
        /// Explicit buffer number for edge commands, relative count otherwise.
        count: Option<usize>,
    },
    /// Jump to a version-matched diagnostic in the active buffer.
    BracketDiagnostic {
        /// Move forward or select the last diagnostic.
        forward: bool,
        /// Select the requested endpoint rather than wrapping around the buffer.
        edge: bool,
        /// Number of positions to move when navigating relatively.
        count: usize,
    },
    /// Open a path or URL with its system handler.
    OpenExternal(String),
    /// Navigate tabs using a direction and an optional one-based count.
    TabNavigate(i32, Option<usize>),
    /// Open a project tabpage showing the current buffer or the given file.
    TabNew(Option<String>),
    /// Close the current project tabpage.
    TabClose,
    /// Close the app, optionally discarding unsaved buffers.
    Close(bool),
    /// Save the active file, then request closing the app.
    SaveClose,
    /// Format the current document through its selected plugin provider.
    Format(Option<Range<usize>>),
    /// Cancel the current manual format request.
    CancelFormat,
    UndoPlugin,
    PluginComplete,
    PluginDiagnose,
    /// Find the definition of the symbol at the cursor.
    PluginDefinition,
    /// Show the document symbols contributed by the active language plugin.
    PluginOutline,
    /// Toggle the fold at the cursor in the current editor viewport.
    PluginFold,
    /// Invoke a declared plugin command by ID.
    PluginCommand(String),
    /// Update an editor-wide appearance preference through the native shell.
    Appearance(String, String),
    /// Run the shell command with the given document range as standard input.
    Filter {
        /// Range of complete lines to replace on success.
        range: Range<usize>,
        /// User-entered shell command.
        command: String,
    },
}

impl Vim {
    /// Open the `prompt`, retaining the Visual range for Ex commands and the search `count`.
    pub(super) fn open_prompt(&mut self, prompt: char, count: usize) {
        self.command_selection = (prompt == ':').then(|| self.selection()).flatten();
        self.mode = if matches!(prompt, '/' | '?') {
            Mode::Search
        } else {
            Mode::Command
        };
        self.prompt = prompt;
        self.command.clear();
        self.command_pos = 0;
        self.search_count = count;
    }

    /// Insert literal `text` at the prompt's UTF-8 caret.
    pub(super) fn command_insert(&mut self, text: &str) {
        let pos = self.command_cursor();
        self.command.insert_str(pos, text);
        self.command_pos = pos + text.len();
    }

    /// Edit or submit one prompt `key`, using `register` for inserted register contents.
    pub(super) fn command_key(&mut self, key: &str, register: &mut Register) -> Action {
        if matches!(self.pending, Some(super::Pending::RegisterInput { .. })) {
            self.register_input_key(key, register);
            return Action::None;
        }
        let pos = self.command_cursor();
        let left = self.command[..pos]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        let right = pos
            + self.command[pos..]
                .graphemes(true)
                .next()
                .map_or(0, str::len);
        match key {
            "Enter" => {
                let mode = self.mode;
                self.mode = Mode::Normal;
                self.command_pos = 0;
                let command = std::mem::take(&mut self.command);
                let selection = self.command_selection.take();
                if mode == Mode::Search {
                    self.confirm_search(&command, register);
                } else if let Some(range) = self.filter_range.take() {
                    let command = command.trim();
                    let command = if command.is_empty() || command == "!" {
                        self.last_filter.as_str()
                    } else {
                        command
                    };
                    if !command.is_empty() {
                        let command = command.to_owned();
                        self.last_filter.clone_from(&command);
                        return Action::Filter { range, command };
                    }
                } else {
                    if self.g.ex_mode && matches!(command.trim(), "vi" | "visual") {
                        self.g.ex_mode = false;
                        return Action::None;
                    }
                    let action = self.ex(&command, selection);
                    if self.g.ex_mode && !matches!(&action, Action::Close(_) | Action::SaveClose) {
                        self.open_prompt(':', 1);
                    }
                    return action;
                }
            }
            "Left" => self.command_pos = left,
            "Right" => self.command_pos = right,
            "Home" | "Ctrl+b" => self.command_pos = 0,
            "End" | "Ctrl+e" => self.command_pos = self.command.len(),
            "Backspace" | "Ctrl+h" => {
                self.command.replace_range(left..pos, "");
                self.command_pos = left;
                if pos == 0 && self.command.is_empty() {
                    self.mode = Mode::Normal;
                    self.search_operator = None;
                    self.filter_range = None;
                    self.command_selection = None;
                }
            }
            "Delete" => self.command.replace_range(pos..right, ""),
            "Ctrl+u" => {
                self.command.drain(..pos);
                self.command_pos = 0;
            }
            "Ctrl+w" => {
                let head = self.command[..pos].trim_end();
                let start = head
                    .rfind(char::is_whitespace)
                    .map_or(0, |i| i + head[i..].chars().next().unwrap().len_utf8());
                self.command.drain(start..pos);
                self.command_pos = start;
            }
            "Ctrl+r" => self.pending = Some(super::Pending::RegisterInput { modifier: None }),
            _ if key.graphemes(true).count() == 1 => self.command_insert(key),
            _ => {}
        }
        Action::None
    }

    /// Execute Ex `text`, passing its retained Visual `selection` to formatting.
    fn ex(&mut self, text: &str, selection: Option<Range<usize>>) -> Action {
        let text = text.trim();
        if text.is_empty() {
            return Action::None;
        }
        if self.substitute_command(text) {
            return Action::None;
        }
        let (name, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let args = args.trim();
        if self.read_only && matches!(name, "w" | "write" | "w!" | "write!" | "wq") {
            return self.reject_edit();
        }
        match name {
            "w" | "write" | "w!" | "write!" => {
                Action::Save((!args.is_empty()).then(|| args.into()))
            }
            "q" | "quit" | "qa" | "qall" => Action::Close(false),
            "q!" | "quit!" | "qa!" | "qall!" => Action::Close(true),
            "wq" => Action::SaveClose,
            "x" | "exit" if self.doc.modified() => Action::SaveClose,
            "x" | "exit" => Action::Close(false),
            "format" => Action::Format(selection),
            "formatcancel" => Action::CancelFormat,
            "undo-plugin" => Action::UndoPlugin,
            "complete" => Action::PluginComplete,
            "diagnose" => Action::PluginDiagnose,
            "definition" => Action::PluginDefinition,
            "outline" => Action::PluginOutline,
            "fold" => Action::PluginFold,
            "plugin" if !args.is_empty() => Action::PluginCommand(args.into()),
            "theme" | "font" | "fontsize" | "zoom" => Action::Appearance(name.into(), args.into()),
            "e" | "edit" if !args.is_empty() => Action::Open(args.into()),
            "tabnew" | "tabedit" => Action::TabNew((!args.is_empty()).then(|| args.into())),
            "tabclose" if args.is_empty() => Action::TabClose,
            _ => {
                self.message = i18n::format("editor.unsupported_command", &[("command", &text)]);
                Action::None
            }
        }
    }
}
