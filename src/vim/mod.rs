//! Nanobug's modal editor, built over its UTF-8 document and shared registers.

mod bracket_commands;
mod command_prompt;
mod commands;
mod comments;
#[cfg(test)]
mod compatibility;
mod ex_batch;
mod ex_buffer_lifecycle;
mod ex_catalog;
mod ex_files;
mod ex_history;
mod ex_message_modifiers;
mod ex_state;
mod ex_window_modifiers;
mod ex_windows;
mod expression;
mod file_marks;
mod filter;
mod folds;
mod g_commands;
mod help_code;
mod hint;
mod input;
mod insert;
mod insert_argument;
mod jumplist;
mod keymap;
mod mappings;
mod menu;
mod mode;
mod motions;
mod number;
mod objects;
mod operators;
mod options;
mod pattern;
mod register;
mod replay;
mod script;
mod session_export;
pub mod shortcuts;
mod spelling;
mod syntax_selection;
#[cfg(test)]
mod tests;

pub use commands::{Action, ArgumentCommand, BatchScope};
pub use ex_buffer_lifecycle::{BufferCommand, BufferSet};
pub use ex_files::search::{find_directory, find_file};
pub use ex_files::{FileArgument, FileBuffer, FileCommand, FileDestination, FileOperation};
pub use ex_message_modifiers::MessageModifier;
pub use ex_window_modifiers::{WindowModifiers, WindowOrientation, WindowPlacement};
pub use ex_windows::{ExModeChange, ViewInformation, ViewSize, WindowCommand};
pub use file_marks::{FileMarkSnapshot, FileMarks};
pub use filter::{run_ex_shell, run_filter};
pub use hint::{Continuation, SequenceHint, VIEW_CONTINUATIONS};
pub use jumplist::JumpHistory;
pub use mappings::{
    InputMappings, LEADER_KEY, MappingContinuation, PendingMappingHint, TerminalStroke,
};
pub use menu::MenuEntry;
pub use mode::Mode;
pub use motions::{HorizontalScroll, ScreenMotion, ScreenMotionResolver};
pub use options::{VimOptions, WindowOptions};
pub use register::Register;
pub use script::{
    ScriptAutocmd, ScriptEvent, ScriptLine, ScriptProgram, ScriptTabScope, decode_ex_source,
};
pub use session_export::{ExportKind, SessionExport};
pub use spelling::SPELL_SUGGESTION_KEYS;

use crate::document::Document;
use motions::Find;
use pattern::{Search, Substitution};
use replay::Replay;
use spelling::SpellSuggestions;
use std::{collections::BTreeMap, ops::Range};

/// The operator preceding a motion or a text object.
#[derive(Clone, Copy)]
struct Operator {
    /// Command character such as d, y or >.
    key: char,
    /// Explicit count before the operator.
    count: Option<usize>,
    /// Explicit motion shape selected by v, V or Ctrl+v after the operator.
    force: Option<Mode>,
}

impl Operator {
    /// Start operator `key` with its optional preceding `count` and the motion's default shape.
    fn new(key: char, count: Option<usize>) -> Self {
        Self {
            key,
            count,
            force: None,
        }
    }
}

/// A command that needs a second key before it can run.
enum Pending {
    /// An operator waiting for its motion.
    Operator(Operator),
    /// A two-key command, optionally inside an operator.
    Prefix(char, Option<Operator>, Option<usize>),
    /// A mark or replacement character awaited by a g command.
    GArgument(g_commands::Argument),
    /// Insert-mode Ctrl+g awaits an undo or vertical movement command.
    InsertControlG,
    /// Insert-mode Ctrl+x awaits a completion source.
    InsertCompletion,
    /// Insert-mode Ctrl+k or Ctrl+v awaits literal characters or a codepoint.
    InsertArgument(insert_argument::Argument),
    /// Ctrl+\ awaits a mode transition key while retaining the active mode.
    ControlBackslash,
    /// Insert or command-line Ctrl+r, optionally followed by a literal insertion modifier.
    RegisterInput {
        /// Modifier following Ctrl+r, when already entered.
        modifier: Option<char>,
    },
    /// Digits entered after z for sizing or scrolling the viewport.
    ZHeight {
        /// Number entered after z.
        rows: usize,
        /// Count entered before z.
        count: Option<usize>,
    },
    /// The count for the spell list selected by the next zu key.
    SpellUndo(Option<usize>),
    /// The active spell correction candidates and their replacement range.
    SpellSuggestions(SpellSuggestions),
    /// A character argument for find, replace, marks, registers or macros.
    Argument(char, Option<Operator>, Option<usize>),
    /// An operator or Visual selection followed by an inner or around object.
    Object {
        around: bool,
        operator: Option<Operator>,
        count: usize,
    },
}

/// Logical viewport supplied by the native editor for screen-relative commands.
struct View {
    /// First row of the current or requested viewport.
    top: usize,
    /// Number of rows visible in the editor.
    rows: usize,
    /// Physical row capacity before a z height command caps the viewport.
    available_rows: usize,
    /// Explicit half-page distance remembered after a counted Ctrl+u or Ctrl+d.
    half_page: Option<usize>,
    /// Requested top after a scroll command, consumed by the window.
    changed: bool,
    /// Requested maximum editor height in rows.
    height_limit: Option<usize>,
    /// First displayed text column and width of the text viewport.
    left: usize,
    columns: usize,
    /// Horizontal scroll request to apply in the editor.
    horizontal: Option<HorizontalScroll>,
}

/// Text and caret saved before a line change, used by U.
struct LineUndo {
    /// Zero-based row and the byte column of its saved caret.
    row: usize,
    column: usize,
    /// Original contents without the line separator.
    text: String,
}

impl Default for View {
    fn default() -> Self {
        Self {
            top: 0,
            rows: 24,
            available_rows: 24,
            half_page: None,
            changed: false,
            height_limit: None,
            left: 0,
            columns: 80,
            horizontal: None,
        }
    }
}

/// Modal state and text for a single buffer. File I/O belongs to the host.
pub struct Vim {
    /// UTF-8 document and caret.
    pub doc: Document,
    /// Position history used by the g family.
    g: g_commands::State,
    /// Whether this buffer accepts navigation and yanks only.
    read_only: bool,
    /// Read-only Vim Help buffer with executable example blocks.
    help_buffer: bool,
    /// Current editing mode.
    pub mode: Mode,
    /// Command or pattern being entered, without the prompt.
    pub command: String,
    /// Visual range retained while an Ex command is being entered.
    command_selection: Option<Range<usize>>,
    /// Visual or Select state when the current Ex prompt was opened.
    command_menu_origin: Option<menu::Origin>,
    /// Last feedback shown in the status line.
    pub message: String,
    /// Whether the latest programmatic Ex command reported an error.
    ex_failed: bool,
    /// Fixed Visual endpoint in UTF-8 bytes.
    pub anchor: usize,
    /// Resume Select mode once its one temporary Visual command has completed.
    select_resume: bool,
    /// Buffer-local definitions and per-mode input mapping state.
    input_maps: mappings::State,
    /// Defaults and global input preferences for this buffer.
    options: InputMappings,
    /// Options shared by all views of the same buffer, initialized from the workspace defaults.
    buffer_options: Option<std::sync::Arc<std::sync::RwLock<options::BufferOptions>>>,
    /// Window-local settings retained when this view's file label is inactive.
    view_options: Option<WindowOptions>,
    /// Visible prompt and the byte position within its input.
    prompt: char,
    command_pos: usize,
    /// Candidate and history cursors for the current prompt.
    command_prompt: command_prompt::State,
    /// Nested expression input and the command or mode it temporarily replaces.
    expression_prompt: Option<expression::Prompt>,
    /// Pending command and its numeric prefix.
    pending: Option<Pending>,
    count: Option<usize>,
    /// Explicit register for the current Normal or Visual command.
    selected_register: Option<char>,
    /// Active Insert or Replace session.
    insert: insert::Session,
    /// Insert or Replace session suspended for one Normal-mode command.
    insert_resume: Option<insert::Resume>,
    /// Preferred display column retained during vertical movement.
    desired_column: Option<usize>,
    /// Preferred horizontal position in measured units across consecutive screen-line motions.
    screen_column: Option<usize>,
    /// Opening brace byte anchoring a caret drawn on the folded preview's closing brace.
    folded_closing_caret: Option<usize>,
    /// Saved character-find arguments and last accepted search/substitution.
    last_find: Option<Find>,
    last_search: Option<Search>,
    /// Temporarily suppress search highlights until the next accepted search.
    search_highlights_hidden: bool,
    last_substitution: Option<Substitution>,
    /// A confirmed substitution owns keys until its remaining matches are resolved.
    substitute_confirmation: Option<pattern::Confirmation>,
    /// Original lines waiting for an Ex command and the revision of its first edit.
    ex_lines: Option<ex_batch::Lines>,
    /// Command-line input collection for :append, :change and :insert.
    ex_line_input: Option<ex_batch::LineInput>,
    /// Pane addressed by the interactive :options scratch view.
    options_editor: Option<usize>,
    /// Original direction and repeat count of the prompt being edited.
    search_count: usize,
    /// Operator that will use the next submitted search as its motion.
    search_operator: Option<Operator>,
    /// Range selected by a pending shell filter, expressed as bytes.
    filter_range: Option<Range<usize>>,
    /// Last shell filter command for :!! and !! followed by an empty prompt.
    last_filter: String,
    /// Buffer-local marks, in UTF-8 bytes.
    marks: BTreeMap<char, usize>,
    /// Individually deleted special marks until their next normal update.
    ex_hidden_marks: std::collections::BTreeSet<char>,
    /// Byte range affected by the latest completed operator or put.
    operated: Option<Range<usize>>,
    /// Jump locations shared with other buffers in this window.
    jump_history: JumpHistory,
    /// Original text of the most recently edited line, used by U.
    line_undo: Option<LineUndo>,
    /// Whether the final LF represents the file terminator.
    final_newline: bool,
    /// Logical viewport for screen motions and scroll effects.
    view: View,
    /// Last completed change and active replay capture.
    replay: Replay,
    /// Named syntax selection ranges and any outstanding host parser request.
    syntax_selection: syntax_selection::State,
}

impl Vim {
    /// Wrap `doc` in Normal mode at its existing caret.
    pub fn new(doc: Document) -> Self {
        let final_newline = doc.text.ends_with('\n');
        Self {
            doc,
            g: g_commands::State::default(),
            read_only: false,
            help_buffer: false,
            mode: Mode::Normal,
            command: String::new(),
            command_selection: None,
            command_menu_origin: None,
            message: String::new(),
            ex_failed: false,
            anchor: 0,
            select_resume: false,
            input_maps: mappings::State::default(),
            options: InputMappings::default(),
            buffer_options: None,
            view_options: None,
            prompt: ':',
            command_pos: 0,
            command_prompt: command_prompt::State::default(),
            expression_prompt: None,
            pending: None,
            count: None,
            selected_register: None,
            insert: insert::Session::default(),
            insert_resume: None,
            desired_column: None,
            screen_column: None,
            folded_closing_caret: None,
            last_find: None,
            last_search: None,
            search_highlights_hidden: false,
            last_substitution: None,
            substitute_confirmation: None,
            ex_lines: None,
            ex_line_input: None,
            options_editor: None,
            search_count: 1,
            search_operator: None,
            filter_range: None,
            last_filter: String::new(),
            marks: BTreeMap::new(),
            ex_hidden_marks: std::collections::BTreeSet::new(),
            operated: None,
            jump_history: JumpHistory::default(),
            line_undo: None,
            final_newline,
            view: View::default(),
            replay: Replay::default(),
            syntax_selection: syntax_selection::State::default(),
        }
    }

    /// Edit arbitrary scratch text, including the blank row after a trailing separator.
    pub fn scratch(doc: Document) -> Self {
        let mut vim = Self::new(doc);
        vim.final_newline = false;
        vim
    }

    /// Wrap a read-only snapshot in Normal mode, retaining navigation, search and yanks.
    pub fn read_only(doc: Document) -> Self {
        let mut vim = Self::new(doc);
        vim.read_only = true;
        vim
    }

    /// Report whether this Vim model presents an immutable source snapshot.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Report a blocked edit without requesting a host action.
    fn reject_edit(&mut self) -> Action {
        self.ex_failed = true;
        self.message = crate::i18n::text("editor.read_only_preview").into();
        Action::None
    }

    /// Prompt glyph for the current command or search.
    pub fn prompt(&self) -> char {
        self.prompt
    }

    /// Byte position of the caret in the current command.
    pub fn command_cursor(&self) -> usize {
        self.command
            .floor_char_boundary(self.command_pos.min(self.command.len()))
    }

    /// Supply viewport height in `rows` and its first displayed row `top` for screen commands.
    pub fn set_view(&mut self, top: usize, rows: usize) {
        let rows = rows.max(1);
        self.view.available_rows = rows;
        let rows = self.view.height_limit.map_or(rows, |limit| rows.min(limit));
        self.view.top = top;
        self.view.rows = rows;
        self.view.half_page = self.view.half_page.map(|lines| lines.min(rows));
        self.view.changed = false;
    }

    /// Return and consume the requested viewport origin in displayed rows.
    pub fn take_view_top(&mut self) -> Option<usize> {
        std::mem::take(&mut self.view.changed).then_some(self.view.top)
    }

    /// Whether a command prefix must consume the next key before workspace mappings.
    pub fn awaiting_key(&self) -> bool {
        self.substitute_confirmation_active()
            || self.pending.is_some()
            || self.count.is_some()
            || self.selected_register.is_some()
            || self.input_maps.has_prefix()
    }

    /// Take a command-mode count for a host window command if Vim owns no other pending input.
    pub fn take_window_count(&mut self) -> Option<Option<usize>> {
        if self.substitute_confirmation_active()
            || self.accepts_text()
            || self.pending.is_some()
            || self.selected_register.is_some()
            || self.input_maps.has_prefix()
        {
            return None;
        }
        let count = self.count.take();
        self.replay.sequence.clear();
        Some(count)
    }

    /// Whether the pending command accepts a single literal grapheme from native input.
    pub fn awaits_literal_argument(&self) -> bool {
        matches!(
            self.pending,
            Some(
                Pending::Argument('f' | 'F' | 't' | 'T' | 'r', ..)
                    | Pending::GArgument(g_commands::Argument::Replace(_))
                    | Pending::InsertArgument(_)
            )
        )
    }

    /// Whether native text belongs to an editable field or a pending literal argument.
    pub fn accepts_text(&self) -> bool {
        !self.substitute_confirmation_active()
            && (self.awaits_literal_argument()
                || (self.mode.accepts_text() || self.g.select) && self.pending.is_none())
    }

    /// Clear an incomplete Normal command before leaving the editor's input layer.
    pub fn cancel_pending(&mut self) {
        self.cancel_substitute_confirmation();
        self.cancel_syntax_request();
        self.input_maps.clear_prefix();
        self.pending = None;
        self.select_resume = false;
        self.search_operator = None;
        self.count = None;
        self.selected_register = None;
        self.replay.sequence.clear();
    }

    /// Adopt text edited in another view and discard operators targeting its old revision.
    pub fn refresh_shared_document(&mut self) -> bool {
        if !self.doc.refresh_shared() {
            return false;
        }
        self.revalidate_document();
        true
    }

    /// Restore saved text and clear operations targeting the discarded revision.
    pub fn discard_changes(&mut self) {
        if self.doc.discard_changes() {
            self.revalidate_document();
        }
    }

    /// Clamp view selections and cancel pending operators when their source text changes.
    fn revalidate_document(&mut self) {
        self.anchor = self
            .doc
            .text
            .floor_char_boundary(self.anchor.min(self.doc.text.len()));
        self.command_selection = None;
        self.filter_range = None;
        self.operated = None;
        self.line_undo = None;
        self.cancel_pending();
    }

    /// Number of logical lines, excluding the empty slot after the file's terminating LF.
    pub fn line_count(&self) -> usize {
        (self.doc.starts.len() - usize::from(self.final_newline && self.doc.text.ends_with('\n')))
            .max(1)
    }

    /// Inclusive Visual selection expressed as one half-open byte range.
    pub fn selection(&self) -> Option<Range<usize>> {
        let (a, b) = (
            self.anchor.min(self.doc.cursor),
            self.anchor.max(self.doc.cursor),
        );
        match self.mode {
            Mode::Visual => Some(a..self.doc.next(b)),
            Mode::VisualLine => {
                let first = self.doc.location(a).0;
                let last = self.doc.location(b).0;
                Some(self.line_span(first, last))
            }
            _ => None,
        }
    }

    /// Selected bytes on `row`, including a rectangular Visual selection.
    pub fn selection_on_line(&self, row: usize) -> Option<Range<usize>> {
        if self.mode != Mode::VisualBlock {
            return self.selection();
        }
        let (rows, columns) = self.block_bounds();
        rows.contains(&row).then(|| self.block_range(row, &columns))
    }

    /// Inclusive range of lines and half-open display columns of the Visual block.
    fn block_bounds(&self) -> (std::ops::RangeInclusive<usize>, Range<usize>) {
        let endpoint = |byte| {
            let (row, left) = self.doc.location(byte);
            let next = self.doc.next(byte).min(self.doc.line_range(row).end);
            let right = if next > byte {
                self.doc.location(next).1
            } else {
                left + 1
            };
            (row, left, right)
        };
        let (a, l1, r1) = endpoint(self.anchor);
        let (b, mut l2, mut r2) = endpoint(self.doc.cursor);
        if let Some(goal) = self
            .desired_column
            .filter(|&goal| goal != usize::MAX && goal > l2)
        {
            l2 = goal;
            r2 = goal + 1;
        }
        (a.min(b)..=a.max(b), l1.min(l2)..r1.max(r2))
    }

    /// Return the graphemes on `row` intersecting the selected display `columns`.
    fn block_range(&self, row: usize, columns: &Range<usize>) -> Range<usize> {
        let start = self.doc.offset(row, columns.start);
        let end = self.doc.offset(row, columns.end);
        let end = if self.doc.location(end).1 < columns.end {
            self.doc.next(end).min(self.doc.line_range(row).end)
        } else {
            end
        };
        start..end
    }

    /// Byte span of the complete lines from `first` through `last`.
    fn line_span(&self, first: usize, last: usize) -> Range<usize> {
        self.doc.starts[first]
            ..self
                .doc
                .starts
                .get(last + 1)
                .copied()
                .unwrap_or(self.doc.text.len())
    }

    /// Change `range` to `text`, maintaining marks and the local U checkpoint.
    fn replace(&mut self, range: Range<usize>, text: &str, checkpoint: bool) {
        if self.read_only || self.doc.text[range.clone()] == *text {
            return;
        }
        let row = self.doc.location(range.start).0;
        if self.line_undo.as_ref().is_none_or(|saved| saved.row != row) {
            let cursor_row = self.doc.location(self.doc.cursor).0;
            self.line_undo = Some(LineUndo {
                row,
                column: if cursor_row == row {
                    self.doc.cursor - self.doc.starts[row]
                } else {
                    0
                },
                text: self.doc.line(row).to_owned(),
            });
        }
        for position in self.marks.values_mut() {
            if *position >= range.end {
                *position = *position - range.len() + text.len();
            } else if *position >= range.start {
                *position = range.start;
            }
        }
        self.g.shift_positions(&range, text.len());
        let checkpoint = self
            .ex_lines
            .as_mut()
            .map_or(checkpoint, |lines| lines.replace(&self.doc, &range, text));
        let checkpoint = self.replay.checkpoint(self.doc.revision, checkpoint);
        self.doc.replace(range.clone(), text, checkpoint);
        self.jump_history.shift(&self.doc, range, text.len());
    }
}
