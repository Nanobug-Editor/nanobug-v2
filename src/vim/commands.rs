//! Native command line and typed requests for file, window or shell operations.

use super::{Mode, Register, ScreenMotionResolver, Vim};
use crate::i18n;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Effects requested by modal editing and executed by the host.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Change the Session buffer list or the current window's argument list.
    BufferLifecycle(super::BufferCommand),
    /// Read or write the undo archive for the current buffer.
    UndoFile {
        path: String,
        write: bool,
        force: bool,
    },
    /// Apply placement and orientation to one Ex command.
    WindowModified {
        modifiers: super::WindowModifiers,
        command: String,
    },
    /// Scope visibility or filtering to one nested Ex command.
    MessageModified {
        modifier: super::MessageModifier,
        command: String,
    },
    /// Build an index for local Vim help documents.
    HelpTags {
        directory: String,
        list_self: bool,
    },
    /// Show diagnostics for the selected components of Nanobug.
    CheckHealth(String),
    /// Discard the workspace and exit using the supplied process status.
    QuitWithCode(i32),
    /// Source scripts matching local runtime path patterns.
    Runtime {
        patterns: String,
        all_matches: bool,
    },
    /// Relaunch the executable after applying an Ex close policy.
    Restart {
        close: String,
        command: String,
    },
    /// Export or restore a view, a Session or Vim settings in portable Ex form.
    SessionExport(super::SessionExport),
    /// No external action.
    None,
    /// Ask the host to parse the current document for an outstanding syntax selection.
    SyntaxSelection,
    /// Open the editable command-line history for the given prompt character.
    CommandWindow(char),
    /// Start a terminal buffer with an optional shell command.
    TerminalOpen(Option<String>),
    /// Open a read-only Vim Help buffer for the given topic or path.
    Help(String),
    /// Open a native popup at the given menu-tree path.
    VimMenu {
        /// Canonical name components from the root to the submenu.
        path: Vec<String>,
    },
    /// Reload numbered marks, or first write the active caret when true.
    NumberedMarks {
        write: bool,
    },
    /// Transfer the portable history to or from an explicitly selected file.
    VimInfo {
        write: bool,
        force: bool,
        path: Option<String>,
    },
    /// Pause modal input for the specified number of seconds.
    Sleep(u64),
    /// Save through the editor's configured formatting shortcut.
    Save(Option<String>),
    /// Write an Ex destination with optional overwrite and window close.
    WriteFile {
        name: Option<String>,
        force: bool,
        close: bool,
    },
    /// Write the current text and give the buffer a new filename.
    SaveAs {
        path: String,
        force: bool,
    },
    /// Write all modified buffers that already have filenames.
    WriteAll,
    /// Write all application buffers, then quit after successful writes.
    WriteQuitAll,
    /// Read a UTF-8 file and insert its lines after a one-based address (zero prepends).
    ReadFile {
        after: usize,
        path: String,
    },
    /// Save the workspace's shared Vim rules and settings to portable storage.
    SaveVimConfig,
    /// Display the current Session's unique retained buffers.
    ListBuffers,
    /// Select a retained buffer by display number or filename.
    SelectBuffer(String),
    /// Select an offset or endpoint in the Session's listed buffers.
    NavigateBuffer {
        direction: i32,
        count: usize,
        edge: bool,
    },
    /// Close a retained buffer by display number or filename, optionally discarding changes.
    DeleteBuffer {
        selector: String,
        force: bool,
    },
    /// Run the existing window command specified by `keys` with an optional leading count.
    WindowCommand {
        keys: String,
        count: Option<usize>,
    },
    /// Open a file or project directory, relative to the current project when needed.
    Open(String),
    /// Jump to an existing file, optionally at a zero-based line and UTF-8 byte column.
    OpenFile(String, Option<(usize, usize)>),
    /// Traverse this window's jumplist across retained or reopened files.
    JumpHistory {
        /// Whether to seek a more recent position.
        forward: bool,
        /// Number of positions to move.
        count: usize,
    },
    /// Display an Ex listing in a scrollable read-only view.
    ShowListing(String),
    /// Open the generated text in its own editable buffer.
    EditableListing(String),
    /// Open a match from the local include tree in a new or preview window.
    IncludeSplit {
        path: Option<std::path::PathBuf>,
        row: usize,
        column: usize,
        preview: bool,
    },
    /// Execute a workspace-dependent file or argument command.
    Files(super::FileCommand),
    /// Execute an Ex window, display or help command.
    WindowExtras(super::WindowCommand),
    /// Run a Shell command and display its output without filtering the buffer.
    Shell(String),
    /// Run the register contents as a sequence of Ex commands.
    RegisterEx(String),
    /// Execute expanded user-command text in the script that defined it.
    UserEx {
        text: String,
        source: Option<String>,
    },
    /// Run the matching automatic commands in one or all retained buffers.
    Autocmd {
        event: String,
        pattern: Option<String>,
        all: bool,
        group: Option<String>,
    },
    /// Execute the enclosed command with automatic event handlers disabled.
    NoAutocmd(String),
    /// Read the system clipboard and execute it as Ex input.
    ClipboardEx,
    /// Remove the supplied numbered mark slots from portable history.
    DeleteNumberedMarks(Vec<usize>),
    /// Stop reading the active :source file.
    FinishSource,
    /// Display the paths of scripts sourced so far.
    ListScripts,
    /// Edit a script by its :scriptnames address or explicit path.
    EditSourcedScript {
        address: Option<usize>,
        path: Option<String>,
    },
    /// Execute Ex statements from a file or the supplied buffer range.
    Source {
        file: Option<String>,
        lines: Option<Range<usize>>,
    },
    /// Apply one Ex command sequence to each item in the selected scope.
    RunBatch {
        scope: BatchScope,
        range: String,
        commands: String,
    },
    /// Undo the last batch in the current Session.
    UndoBatch,
    /// Return to the alternate file or select an explicitly numbered buffer.
    AlternateBuffer(Option<u64>),
    /// Open a mark's file in the current editor and position the caret.
    OpenFileMark {
        /// Uppercase mark stored across buffers.
        name: char,
        /// Last named file path, when the mark belongs to a file-backed buffer.
        path: Option<std::path::PathBuf>,
        /// Retained buffer number, or zero for a disk-only mark.
        opening: u64,
        /// Zero-based physical line from when the mark was created.
        row: usize,
        /// Byte column for uppercase marks or display column for numbered marks.
        column: usize,
        /// Whether to jump to the first nonblank on the marked line.
        linewise: bool,
    },
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
    /// Select the first or last editor tabpage.
    TabEndpoint(bool),
    /// Move the current Tabpage after an absolute or relative address.
    TabMove(String),
    /// List the current Session's tabpages and editor windows.
    ListTabs,
    /// Open a project tabpage showing a blank buffer or the given file.
    TabNew(Option<String>),
    /// Close the current project tabpage.
    TabClose,
    /// Close sibling tabpages after resolving modified buffers.
    TabOnly,
    /// Close the other splits in the current tabpage.
    Only,
    /// Replace the current display with an empty, unnamed buffer.
    EmptyBuffer,
    /// Open an empty editor split.
    EmptySplit(bool),
    /// Apply an argument-list command with optional address and raw filenames.
    Arguments {
        /// Operation to execute on the project's shared list.
        command: ArgumentCommand,
        /// Optional Ex address or bracket count.
        count: Option<usize>,
        /// Unparsed arguments, including escaped whitespace in paths.
        arguments: String,
    },
    /// Open this filename in the current tabpage's preview split without moving focus.
    PreviewOpen(String),
    /// Close the preview split using the usual modified-buffer confirmation.
    PreviewClose,
    /// Open another editor window in the current tabpage.
    Split {
        /// Place the new editor above or below the current editor when true.
        stacked: bool,
        /// File to open in the split, or the current file when omitted.
        file: Option<String>,
    },
    /// Close the active editor, optionally discarding its unsaved buffers.
    Close(bool),
    /// Close the active editor window only.
    CloseWindow(bool),
    /// Quit the application, optionally discarding every unsaved buffer.
    QuitAll(bool),
    /// Save the active file, then request closing its editor.
    SaveClose,
    /// Format the current document through its selected plugin provider.
    Format(Option<Range<usize>>),
    /// Select the project SQL dialect or override it for the current buffer.
    SqlDialect {
        /// An Ex bang limits the selection to the open buffer.
        buffer_only: bool,
        /// Manifest ID, `auto` to reset, or empty for the command usage.
        name: String,
    },
    /// Cancel the current manual format request.
    CancelFormat,
    UndoPlugin,
    PluginComplete,
    /// Present native Insert completions replacing the typed prefix at the caret.
    InsertCandidates {
        /// Byte offset immediately before the typed prefix.
        start: usize,
        /// Matching file paths, lines or words in selection order.
        values: Vec<String>,
        /// Start at the final candidate for a backward word completion.
        reverse: bool,
    },
    PluginDiagnose,
    /// Find the definition of the symbol at the cursor.
    PluginDefinition,
    /// Show the signature, type and documentation for the symbol at the cursor.
    PluginHover,
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

/// The object enumerated by an Ex list command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchScope {
    Arguments,
    Buffers,
    Windows,
    Tabpages,
}

/// Ex or bracket operations on the project's argument list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgumentCommand {
    /// Show the list, or replace it and open the first argument when given filenames.
    List,
    /// Insert files after the current or addressed argument, optionally editing the first item.
    Add { edit: bool },
    /// Delete the specified entries, or the current one when omitted.
    Delete,
    /// Keep the first occurrence of each argument.
    Deduplicate,
    /// Open an argument by its one-based index, or reopen the current one.
    At,
    /// Move relatively in list order, or replace it with new filenames on `:next`.
    Step { forward: bool },
    /// Open the first or last argument.
    Edge { last: bool },
}

/// Commands understood by the Ex dispatcher.
#[derive(Clone, Copy)]
pub(super) enum ExCommand {
    Write,
    SaveAs,
    Update,
    Wall,
    WriteQuitAll,
    Read,
    VimConfig,
    Buffers,
    Buffer,
    BufferNext,
    BufferPrevious,
    BufferFirst,
    BufferLast,
    BufferDelete,
    WindowCommand,
    Registers,
    Marks,
    Changes,
    UndoList,
    History,
    Source,
    RunBatch(BatchScope),
    UndoBatch,
    Close,
    CloseWindow,
    QuitAll,
    WriteQuit,
    Exit,
    Format,
    SqlDialect,
    CancelFormat,
    UndoPlugin,
    Complete,
    Diagnose,
    Definition,
    Outline,
    Fold,
    Plugin,
    Theme,
    Font,
    FontSize,
    Zoom,
    Edit,
    Jumps,
    ClearJumps,
    TabNew,
    TabEdit,
    TabClose,
    TabOnly,
    TabNext,
    TabPrevious,
    TabFirst,
    TabLast,
    TabMove,
    Tabs,
    New,
    VerticalNew,
    EmptyNew,
    Only,
    Split,
    VerticalSplit,
    Substitute,
    Batch,
    BatchBang,
    Script,
    Help,
    NumberedMarks(bool),
    ArgumentList(ArgumentCommand),
    PreviewEdit,
    PreviewClose,
    Terminal,
    Set,
}

/// Command spelling shared by dispatch and Tab completion.
struct ExName {
    /// Canonical spelling offered by completion.
    name: &'static str,
    /// Additional abbreviated spelling already accepted by the dispatcher.
    alias: Option<&'static str>,
    /// Operation selected by this spelling.
    command: ExCommand,
}

impl ExName {
    /// Describe `command` with its full `name` and optional abbreviated `alias`.
    const fn new(command: ExCommand, name: &'static str, alias: Option<&'static str>) -> Self {
        Self {
            name,
            alias,
            command,
        }
    }
}

/// The accepted Ex spellings, including substitution which parses its range and delimiters separately.
const EX_NAMES: &[ExName] = {
    use ExCommand::*;
    &[
        ExName::new(Set, "set", Some("se")),
        ExName::new(Write, "write", Some("w")),
        ExName::new(SaveAs, "saveas", Some("sav")),
        ExName::new(Update, "update", Some("up")),
        ExName::new(Wall, "wall", Some("wa")),
        ExName::new(WriteQuitAll, "wqall", Some("wqa")),
        ExName::new(WriteQuitAll, "xall", Some("xa")),
        ExName::new(Read, "read", Some("r")),
        ExName::new(VimConfig, "vimconfig", None),
        ExName::new(Buffers, "buffers", Some("ls")),
        ExName::new(Buffers, "files", None),
        ExName::new(Buffer, "buffer", Some("b")),
        ExName::new(BufferNext, "bnext", Some("bn")),
        ExName::new(BufferPrevious, "bprevious", Some("bp")),
        ExName::new(BufferPrevious, "bNext", Some("bN")),
        ExName::new(BufferFirst, "bfirst", Some("bf")),
        ExName::new(BufferLast, "blast", Some("bl")),
        ExName::new(BufferDelete, "bdelete", Some("bd")),
        ExName::new(WindowCommand, "wincmd", Some("winc")),
        ExName::new(Registers, "registers", Some("reg")),
        ExName::new(Registers, "display", Some("di")),
        ExName::new(Marks, "marks", None),
        ExName::new(Changes, "changes", None),
        ExName::new(UndoList, "undolist", Some("undol")),
        ExName::new(History, "history", Some("his")),
        ExName::new(Source, "source", Some("so")),
        ExName::new(RunBatch(BatchScope::Arguments), "argdo", None),
        ExName::new(RunBatch(BatchScope::Buffers), "bufdo", None),
        ExName::new(RunBatch(BatchScope::Windows), "windo", None),
        ExName::new(RunBatch(BatchScope::Tabpages), "tabdo", None),
        ExName::new(UndoBatch, "undo-batch", None),
        ExName::new(Close, "quit", Some("q")),
        ExName::new(CloseWindow, "close", Some("clo")),
        ExName::new(QuitAll, "qall", Some("qa")),
        ExName::new(WriteQuit, "wq", None),
        ExName::new(Exit, "exit", Some("x")),
        ExName::new(Format, "format", None),
        ExName::new(SqlDialect, "sqldialect", None),
        ExName::new(CancelFormat, "formatcancel", None),
        ExName::new(UndoPlugin, "undo-plugin", None),
        ExName::new(Complete, "complete", None),
        ExName::new(Diagnose, "diagnose", None),
        ExName::new(Definition, "definition", None),
        ExName::new(Outline, "outline", None),
        ExName::new(Fold, "fold", None),
        ExName::new(Plugin, "plugin", None),
        ExName::new(Theme, "theme", None),
        ExName::new(Font, "font", None),
        ExName::new(FontSize, "fontsize", None),
        ExName::new(Zoom, "zoom", None),
        ExName::new(Edit, "edit", Some("e")),
        ExName::new(Jumps, "jumps", Some("ju")),
        ExName::new(ClearJumps, "clearjumps", Some("cle")),
        ExName::new(TabNew, "tabnew", None),
        ExName::new(TabEdit, "tabedit", None),
        ExName::new(TabClose, "tabclose", None),
        ExName::new(TabOnly, "tabonly", Some("tabo")),
        ExName::new(TabNext, "tabnext", Some("tabn")),
        ExName::new(TabPrevious, "tabprevious", Some("tabp")),
        ExName::new(TabPrevious, "tabNext", Some("tabN")),
        ExName::new(TabFirst, "tabfirst", Some("tabfir")),
        ExName::new(TabLast, "tablast", Some("tabl")),
        ExName::new(TabMove, "tabmove", Some("tabm")),
        ExName::new(Tabs, "tabs", None),
        ExName::new(New, "new", None),
        ExName::new(VerticalNew, "vnew", None),
        ExName::new(EmptyNew, "enew", Some("ene")),
        ExName::new(Only, "only", Some("on")),
        ExName::new(Split, "split", Some("sp")),
        ExName::new(VerticalSplit, "vsplit", Some("vs")),
        ExName::new(Substitute, "substitute", Some("s")),
        ExName::new(BatchBang, "global", Some("g")),
        ExName::new(Batch, "vglobal", Some("v")),
        ExName::new(Batch, "delete", Some("d")),
        ExName::new(Batch, "yank", Some("y")),
        ExName::new(Batch, "copy", Some("co")),
        ExName::new(Batch, "move", Some("m")),
        ExName::new(BatchBang, "sort", Some("sor")),
        ExName::new(Batch, "print", Some("p")),
        ExName::new(Batch, "number", Some("nu")),
        ExName::new(Batch, "list", Some("l")),
        ExName::new(BatchBang, "normal", Some("norm")),
        ExName::new(Script, "function", Some("fu")),
        ExName::new(Script, "endfunction", Some("endf")),
        ExName::new(Script, "echo", None),
        ExName::new(Script, "echomsg", Some("echom")),
        ExName::new(Script, "call", None),
        ExName::new(Help, "help", Some("h")),
        ExName::new(NumberedMarks(true), "wshada", Some("wsh")),
        ExName::new(NumberedMarks(false), "rshada", Some("rsh")),
        ExName::new(ArgumentList(ArgumentCommand::List), "args", Some("ar")),
        ExName::new(
            ArgumentList(ArgumentCommand::Add { edit: false }),
            "argadd",
            Some("arga"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Add { edit: true }),
            "argedit",
            Some("arge"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Delete),
            "argdelete",
            Some("argd"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Deduplicate),
            "argdedupe",
            Some("argded"),
        ),
        ExName::new(ArgumentList(ArgumentCommand::At), "argument", Some("argu")),
        ExName::new(
            ArgumentList(ArgumentCommand::Step { forward: true }),
            "next",
            Some("n"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Step { forward: false }),
            "previous",
            Some("prev"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Step { forward: false }),
            "Next",
            Some("N"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Edge { last: false }),
            "first",
            Some("fir"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Edge { last: false }),
            "rewind",
            Some("rew"),
        ),
        ExName::new(
            ArgumentList(ArgumentCommand::Edge { last: true }),
            "last",
            Some("la"),
        ),
        ExName::new(PreviewEdit, "pedit", Some("ped")),
        ExName::new(PreviewClose, "pclose", Some("pc")),
        ExName::new(Terminal, "terminal", Some("te")),
    ]
};

impl ExCommand {
    /// Resolve a full or abbreviated `word` and its accepted trailing bang.
    pub(super) fn parse(word: &str) -> Option<(Self, bool)> {
        let (word, bang) = word
            .strip_suffix('!')
            .map_or((word, false), |name| (name, true));
        let name = EX_NAMES.iter().find(|name| {
            (!bang || name.command.takes_bang()) && (name.name == word || name.alias == Some(word))
        });
        if name.is_none() && word.len() >= 2 && "terminal".starts_with(word) {
            return Some((Self::Terminal, bang));
        }
        let name = name?;
        Some((name.command, bang))
    }

    /// Complete supported canonical names beginning with the typed `prefix`.
    pub(super) fn candidates(prefix: &str) -> Vec<String> {
        let (prefix, bang) = prefix
            .strip_suffix('!')
            .map_or((prefix, false), |name| (name, true));
        EX_NAMES
            .iter()
            .filter(|name| name.name.starts_with(prefix) && (!bang || name.command.takes_bang()))
            .map(|name| {
                if bang {
                    format!("{}!", name.name)
                } else {
                    name.name.into()
                }
            })
            .collect()
    }

    /// Whether this Ex command accepts a file or directory argument.
    pub(super) fn takes_path(self) -> bool {
        matches!(
            self,
            Self::Edit
                | Self::Write
                | Self::SaveAs
                | Self::WriteQuit
                | Self::Update
                | Self::Read
                | Self::Source
                | Self::New
                | Self::VerticalNew
                | Self::TabNew
                | Self::TabEdit
                | Self::Split
                | Self::VerticalSplit
                | Self::PreviewEdit
                | Self::ArgumentList(
                    ArgumentCommand::List
                        | ArgumentCommand::Add { .. }
                        | ArgumentCommand::Delete
                        | ArgumentCommand::Step { forward: true }
                )
        )
    }

    /// Whether this Ex spelling recognizes the bang modifier.
    fn takes_bang(self) -> bool {
        matches!(
            self,
            Self::Write
                | Self::SaveAs
                | Self::Update
                | Self::WriteQuit
                | Self::BufferDelete
                | Self::Close
                | Self::CloseWindow
                | Self::QuitAll
                | Self::SqlDialect
                | Self::Terminal
                | Self::BatchBang
                | Self::RunBatch(BatchScope::Arguments | BatchScope::Buffers)
        )
    }
}

impl Vim {
    /// Run an Ex `line` programmatically using the same dispatch as the command prompt.
    pub fn execute_ex_line(&mut self, line: &str, register: &mut Register) -> Action {
        let line = line.trim_start().trim_start_matches(':').trim_start();
        self.message.clear();
        self.ex_failed = false;
        let mode = self.mode;
        let action = self.execute_ex_command(line, None, None, register, &mut |_, _, _, _| None);
        if super::script::defining_function(register)
            && mode != Mode::Command
            && self.mode == Mode::Command
        {
            self.mode = mode;
            self.command.clear();
        }
        action
    }

    /// Whether a programmatically executed Ex line reported an error.
    pub fn ex_failed(&self) -> bool {
        self.ex_failed
    }

    /// Publish the status text and failure outcome of a host-run nested Ex command.
    pub fn publish_ex_message(&mut self, text: &str, failed: bool) {
        self.message = text.into();
        self.ex_failed = failed;
    }

    /// Split an Ex pipeline into statements, preserving escaped bars and quotes.
    pub fn ex_statements(line: &str) -> impl Iterator<Item = &str> {
        super::script::statements(line)
    }

    /// Preserve command tails that interpret their own command separators in a script.
    pub fn source_statements<'a>(&self, line: &'a str) -> Vec<&'a str> {
        if self
            .ex_range(line)
            .is_some_and(|(_, body)| body.starts_with('!'))
        {
            return vec![line];
        }
        let command = line.trim_start_matches(|ch: char| !ch.is_ascii_alphabetic());
        let name = command
            .split(|ch: char| !(ch.is_ascii_alphabetic() || ch == '!'))
            .next()
            .unwrap_or("");
        if matches!(
            name.trim_end_matches('!'),
            "fu" | "func" | "function" | "g" | "global" | "v" | "vglobal" | "norm" | "normal"
        ) || matches!(ExCommand::parse(name), Some((ExCommand::RunBatch(_), _)))
            || name.len() >= 3 && "command".starts_with(name.trim_end_matches('!'))
            || name.len() >= 1
                && ["append", "change", "insert"]
                    .iter()
                    .any(|verb| verb.starts_with(name.trim_end_matches('!')))
        {
            vec![line]
        } else {
            Self::ex_statements(line).collect()
        }
    }

    /// Open the `prompt`, retaining the Visual range for Ex commands and the search `count`.
    pub(super) fn open_prompt(&mut self, prompt: char, count: usize) {
        self.command_menu_origin = (prompt == ':').then(|| self.menu_origin()).flatten();
        self.command_selection = (prompt == ':').then(|| self.selection()).flatten();
        self.mode = if matches!(prompt, '/' | '?') {
            Mode::Search
        } else {
            Mode::Command
        };
        self.prompt = prompt;
        self.begin_input_prompt();
        self.command.clear();
        self.command_pos = 0;
        self.command_prompt.reset();
        self.command_prompt.overstrike = false;
        self.command_prompt.search_origin = (self.mode == Mode::Search).then_some(self.doc.cursor);
        self.command_prompt.search_preview = None;
        self.search_count = count;
        if prompt == ':' {
            if self.command_selection.is_some() {
                self.command.push_str("'<,'>");
            } else if count > 1 {
                self.command = format!(".,.+{}", count - 1);
            }
            self.command_pos = self.command.len();
        }
    }

    /// Enter literal `text` at the prompt's UTF-8 caret, replacing as many graphemes in overstrike.
    pub(super) fn command_insert(&mut self, text: &str) {
        self.command_prompt.reset();
        let pos = self.command_cursor();
        let end = if self.command_prompt.overstrike {
            self.command[pos..]
                .grapheme_indices(true)
                .nth(text.graphemes(true).count())
                .map_or(self.command.len(), |(at, _)| pos + at)
        } else {
            pos
        };
        self.command.replace_range(pos..end, text);
        self.command_pos = pos + text.len();
        self.preview_search(0);
    }

    /// Edit or submit one prompt `key`, using `register` for inserted register contents.
    pub(super) fn command_key(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if matches!(self.pending, Some(super::Pending::RegisterInput { .. })) {
            self.register_input_key(key, register);
            return Action::None;
        }
        if key == "Ctrl+f" && self.expression_prompt.is_none() {
            return Action::CommandWindow(self.prompt);
        }
        match key {
            "Ctrl+g" | "Ctrl+t" if self.mode == Mode::Search => {
                self.preview_search(if key == "Ctrl+g" { 1 } else { -1 });
                return Action::None;
            }
            "Ctrl+l" if self.mode == Mode::Search => {
                self.extend_prompt_search();
                return Action::None;
            }
            "Tab" | "Ctrl+i" | "Shift+Tab"
                if self.cycle_command_completion(key == "Shift+Tab", true, register) =>
            {
                return Action::None;
            }
            "Ctrl+e" if self.cancel_command_completion() => return Action::None,
            "Ctrl+a" | "Ctrl+d" | "Ctrl+l" if self.mode == Mode::Command && self.prompt == ':' => {
                self.complete_command_group(key, register);
                return Action::None;
            }
            "Ctrl+n" | "Ctrl+p"
                if self.cycle_command_completion(key == "Ctrl+p", false, register) =>
            {
                return Action::None;
            }
            "Up" | "Down" | "Ctrl+n" | "Ctrl+p" | "PageUp" | "PageDown" => {
                self.recall_command(
                    register,
                    matches!(key, "Up" | "Ctrl+p" | "PageUp"),
                    matches!(key, "Up" | "Down"),
                );
                self.preview_search(0);
                return Action::None;
            }
            _ => self.command_prompt.reset(),
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
            "Enter" | "Ctrl+j" | "Ctrl+m" => {
                let mode = self.mode;
                let preview = self.end_prompt_search();
                self.mode = Mode::Normal;
                self.command_pos = 0;
                let command = std::mem::take(&mut self.command);
                if !self.repeats_previous_ex(&command, register) || self.prompt != ':' {
                    register.command_history.remember(self.prompt, &command);
                }
                let selection = self.command_selection.take();
                let menu_origin = self.command_menu_origin.take();
                if mode == Mode::Search {
                    self.confirm_search(&command, preview, register);
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
                    if self.g.ex_mode
                        && matches!(command.trim(), "vi" | "vis" | "visual" | "vie" | "view")
                    {
                        self.g.ex_mode = false;
                        return Action::None;
                    }
                    let action = self.execute_ex_command(
                        &command,
                        selection,
                        menu_origin,
                        register,
                        resolve_screen,
                    );
                    if self.g.ex_mode
                        && !self.substitute_confirmation_active()
                        && !matches!(
                            &action,
                            Action::Close(_)
                                | Action::CloseWindow(_)
                                | Action::QuitAll(_)
                                | Action::QuitWithCode(_)
                                | Action::Restart { .. }
                                | Action::SaveClose
                                | Action::WriteFile { close: true, .. }
                                | Action::WriteQuitAll
                        )
                    {
                        self.open_prompt(':', 1);
                    }
                    return action;
                }
            }
            "Left" => self.command_pos = left,
            "Right" => self.command_pos = right,
            "Ctrl+Left" | "Shift+Left" => {
                let head = self.command[..pos].trim_end();
                self.command_pos = head
                    .rfind(char::is_whitespace)
                    .map_or(0, |at| at + head[at..].chars().next().unwrap().len_utf8());
            }
            "Ctrl+Right" | "Shift+Right" => {
                let rest = &self.command[pos..];
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                self.command_pos = self.command.len() - rest[end..].trim_start().len();
            }
            "Insert" => self.command_prompt.overstrike = !self.command_prompt.overstrike,
            "Home" | "Ctrl+b" => self.command_pos = 0,
            "End" | "Ctrl+e" => self.command_pos = self.command.len(),
            "Backspace" | "Ctrl+h" => {
                self.command.replace_range(left..pos, "");
                self.command_pos = left;
                if pos == 0 && self.command.is_empty() {
                    if self.cancel_expression() {
                        return Action::None;
                    }
                    self.end_prompt_search();
                    self.mode = Mode::Normal;
                    self.search_operator = None;
                    self.filter_range = None;
                    self.command_selection = None;
                } else {
                    self.preview_search(0);
                }
            }
            "Delete" if pos == right => {
                self.command.replace_range(left..pos, "");
                self.command_pos = left;
                self.preview_search(0);
            }
            "Delete" => {
                self.command.replace_range(pos..right, "");
                self.preview_search(0);
            }
            "Ctrl+u" => {
                self.command.drain(..pos);
                self.command_pos = 0;
                self.preview_search(0);
            }
            "Ctrl+w" => {
                let head = self.command[..pos].trim_end();
                let start = head
                    .rfind(char::is_whitespace)
                    .map_or(0, |i| i + head[i..].chars().next().unwrap().len_utf8());
                self.command.drain(start..pos);
                self.command_pos = start;
                self.preview_search(0);
            }
            "Ctrl+r" => self.pending = Some(super::Pending::RegisterInput { modifier: None }),
            "Ctrl+k" | "Ctrl+v" | "Ctrl+q" | "Ctrl+Shift+v" | "Ctrl+Shift+q" => {
                self.begin_literal_input(key);
            }
            _ if key.graphemes(true).count() == 1 => self.command_insert(key),
            _ => {}
        }
        Action::None
    }

    /// Execute Ex `text` for a typed prompt or a <Cmd> mapping.
    /// `selection` and `menu_origin` retain addresses and mode across the prompt.
    pub(super) fn execute_ex_command(
        &mut self,
        text: &str,
        selection: Option<Range<usize>>,
        menu_origin: Option<super::menu::Origin>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if self.configure_options(text, register) || self.configure_input(text, register) {
            return Action::None;
        }
        if let Some(action) = self.menu_command(text, menu_origin, register, resolve_screen) {
            return action;
        }
        self.script_command(text, register)
            .or_else(|| self.ex_state_command(text, register))
            .or_else(|| self.ex_catalog_command(text))
            .or_else(|| self.ex_history_command(text, register, resolve_screen))
            .or_else(|| self.ex_buffer_lifecycle(text))
            .or_else(|| self.session_export_ex_command(text))
            .or_else(|| self.ex_file_command(text))
            .or_else(|| self.ex_message_modifier(text))
            .or_else(|| self.ex_window_command(text, register))
            .or_else(|| self.ex_window_modifiers(text))
            .or_else(|| self.ex_batch(text, register, resolve_screen))
            .unwrap_or_else(|| self.ex(text, selection, register))
    }

    /// Execute Ex `text`, passing its retained Visual `selection` to formatting.
    fn ex(&mut self, text: &str, selection: Option<Range<usize>>, register: &Register) -> Action {
        let text = text.trim();
        if text.is_empty() {
            return Action::None;
        }
        if self.substitute_command(text) {
            return Action::None;
        }
        // A number here addresses the file list, independently of the document's line range.
        let (word, arguments) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let batch = word.trim_start_matches(|ch: char| {
            ch.is_ascii_digit() || matches!(ch, ',' | '%' | '.' | '$')
        });
        if let Some((ExCommand::RunBatch(scope), _)) = ExCommand::parse(batch)
            && !arguments.trim().is_empty()
        {
            return Action::RunBatch {
                scope,
                range: word[..word.len() - batch.len()].into(),
                commands: arguments.trim().into(),
            };
        }
        let name = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
        let count = word[..word.len() - name.len()].parse().ok();
        let arguments = arguments.trim();
        match ExCommand::parse(name) {
            Some((ExCommand::ArgumentList(command), _)) => {
                return Action::Arguments {
                    command,
                    count,
                    arguments: arguments.into(),
                };
            }
            Some((ExCommand::Buffer, _)) if count.is_none() || arguments.is_empty() => {
                return Action::SelectBuffer(
                    count.map_or_else(|| arguments.into(), |id: usize| id.to_string()),
                );
            }
            Some((ExCommand::BufferDelete, force)) if count.is_none() || arguments.is_empty() => {
                return Action::DeleteBuffer {
                    selector: count.map_or_else(|| arguments.into(), |id| id.to_string()),
                    force,
                };
            }
            Some((ExCommand::WindowCommand, _)) if !arguments.is_empty() => {
                return Action::WindowCommand {
                    keys: arguments.into(),
                    count,
                };
            }
            Some((ExCommand::TabMove, _)) if count.is_none() || arguments.is_empty() => {
                return Action::TabMove(
                    count.map_or_else(|| arguments.into(), |value| value.to_string()),
                );
            }
            Some((
                command @ (ExCommand::BufferNext
                | ExCommand::BufferPrevious
                | ExCommand::BufferFirst
                | ExCommand::BufferLast),
                _,
            )) => {
                let edge = matches!(command, ExCommand::BufferFirst | ExCommand::BufferLast);
                if (arguments.is_empty() || count.is_none() && arguments.parse::<usize>().is_ok())
                    && (!edge || count.is_none() && arguments.is_empty())
                {
                    return Action::NavigateBuffer {
                        direction: if matches!(
                            command,
                            ExCommand::BufferNext | ExCommand::BufferLast
                        ) {
                            1
                        } else {
                            -1
                        },
                        count: count.or_else(|| arguments.parse().ok()).unwrap_or(1).max(1),
                        edge,
                    };
                }
            }
            Some((command @ (ExCommand::TabNext | ExCommand::TabPrevious), _))
                if arguments.is_empty()
                    || count.is_none() && arguments.parse::<usize>().is_ok() =>
            {
                let count = count.or_else(|| arguments.parse().ok());
                return if matches!(command, ExCommand::TabNext) {
                    Action::TabNavigate(1, count)
                } else {
                    Action::TabNavigate(-(count.unwrap_or(1).min(i32::MAX as usize) as i32), None)
                };
            }
            _ => {}
        }
        let (address, body) = self.ex_range(text).unwrap_or((None, text));
        let (name, args) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        let args = args.trim();
        use ExCommand::*;
        let parsed = ExCommand::parse(name)
            .filter(|(command, _)| address.is_none() || matches!(command, Format | Read | Source));
        if self.read_only
            && matches!(
                parsed,
                Some((
                    Write | SaveAs | WriteQuit | WriteQuitAll | Update | Wall | Read | Source,
                    _
                ))
            )
        {
            return self.reject_edit();
        }
        match parsed {
            Some((Terminal, _)) => Action::TerminalOpen((!args.is_empty()).then(|| args.into())),
            Some((Help, _)) => Action::Help(args.into()),
            Some((NumberedMarks(write), _)) if args.is_empty() => Action::NumberedMarks { write },
            Some((Set, _)) if self.set_thesaurus(args) => Action::None,
            Some((Write, force)) => Action::WriteFile {
                name: (!args.is_empty()).then(|| args.into()),
                force,
                close: false,
            },
            Some((SaveAs, force)) if !args.is_empty() => Action::SaveAs {
                path: args.into(),
                force,
            },
            Some((Update, force)) => {
                if self.doc.modified() || self.doc.path.as_ref().is_some_and(|path| !path.exists())
                {
                    Action::WriteFile {
                        name: (!args.is_empty()).then(|| args.into()),
                        force,
                        close: false,
                    }
                } else {
                    Action::None
                }
            }
            Some((Wall, _)) if args.is_empty() => Action::WriteAll,
            Some((WriteQuitAll, _)) if args.is_empty() => Action::WriteQuitAll,
            Some((Read, _)) if !args.is_empty() && !args.starts_with('!') => {
                let current = self.doc.location(self.doc.cursor).0 + 1;
                let after = if self
                    .ex_address(text, current)
                    .is_some_and(|(line, tail)| line == 0 && tail.trim_start() == body)
                {
                    0
                } else {
                    address.map_or(current, |range| range.end)
                };
                Action::ReadFile {
                    after,
                    path: args.into(),
                }
            }
            Some((VimConfig, _)) if args == "save" => Action::SaveVimConfig,
            Some((Buffers, _)) if args.is_empty() => Action::ListBuffers,
            Some((Registers, _)) => Action::ShowListing(register.ex_listing(args)),
            Some((Marks, _)) => Action::ShowListing(self.ex_marks(register, args)),
            Some((Changes, _)) if args.is_empty() => {
                Action::ShowListing(self.g.ex_changes(&self.doc))
            }
            Some((UndoList, _)) if args.is_empty() => Action::ShowListing(self.doc.undo_branches()),
            Some((History, _)) => Action::ShowListing(register.command_history.ex_listing(args)),
            Some((Source, _)) if args.is_empty() || address.is_none() => Action::Source {
                file: (!args.is_empty()).then(|| args.into()),
                lines: address,
            },
            Some((UndoBatch, _)) if args.is_empty() => Action::UndoBatch,
            Some((Close, bang)) => Action::Close(bang),
            Some((CloseWindow, bang)) => Action::CloseWindow(bang),
            Some((QuitAll, bang)) => Action::QuitAll(bang),
            Some((WriteQuit, false)) if args.is_empty() => Action::SaveClose,
            Some((WriteQuit, force)) => Action::WriteFile {
                name: (!args.is_empty()).then(|| args.into()),
                force,
                close: true,
            },
            Some((Exit, _)) if self.doc.modified() => Action::SaveClose,
            Some((Exit, _)) => Action::Close(false),
            Some((Format, _)) => Action::Format(
                selection
                    .or_else(|| address.map(|range| self.line_span(range.start, range.end - 1))),
            ),
            Some((SqlDialect, bang)) => Action::SqlDialect {
                buffer_only: bang,
                name: args.to_ascii_lowercase(),
            },
            Some((CancelFormat, _)) => Action::CancelFormat,
            Some((UndoPlugin, _)) => Action::UndoPlugin,
            Some((Complete, _)) => Action::PluginComplete,
            Some((Diagnose, _)) => Action::PluginDiagnose,
            Some((Definition, _)) => Action::PluginDefinition,
            Some((Outline, _)) => Action::PluginOutline,
            Some((Fold, _)) => Action::PluginFold,
            Some((Plugin, _)) if !args.is_empty() => Action::PluginCommand(args.into()),
            Some((Theme | Font | FontSize | Zoom, _)) => {
                Action::Appearance(name.into(), args.into())
            }
            Some((Edit, _)) if args == "#" => Action::AlternateBuffer(None),
            Some((Edit, _))
                if let Some(id) = args
                    .strip_prefix('#')
                    .and_then(|number| number.parse().ok()) =>
            {
                Action::AlternateBuffer(Some(id))
            }
            Some((Edit, _)) if !args.is_empty() => Action::Open(args.into()),
            Some((Jumps, _)) if args.is_empty() => {
                Action::ShowListing(self.jump_history.list(&self.doc))
            }
            Some((ClearJumps, _)) if args.is_empty() => {
                self.jump_history.clear();
                Action::None
            }
            Some((TabNew | TabEdit, _)) => Action::TabNew((!args.is_empty()).then(|| args.into())),
            Some((TabClose, _)) if args.is_empty() => Action::TabClose,
            Some((TabOnly, _)) if args.is_empty() => Action::TabOnly,
            Some((TabFirst | TabLast, _)) if args.is_empty() => {
                Action::TabEndpoint(matches!(parsed, Some((TabLast, _))))
            }
            Some((Tabs, _)) if args.is_empty() => Action::ListTabs,
            Some((Only, _)) if args.is_empty() => Action::Only,
            Some((EmptyNew, _)) if args.is_empty() => Action::EmptyBuffer,
            Some((New | VerticalNew, _)) if args.is_empty() => {
                Action::EmptySplit(matches!(parsed, Some((New, _))))
            }
            Some((command @ (Split | VerticalSplit | New | VerticalNew), _)) => Action::Split {
                stacked: matches!(command, Split | New),
                file: (!args.is_empty()).then(|| args.into()),
            },
            Some((PreviewEdit, _)) if !args.is_empty() => Action::PreviewOpen(args.into()),
            Some((PreviewClose, _)) if args.is_empty() => Action::PreviewClose,
            _ => {
                self.ex_failed = true;
                self.message = i18n::format("editor.unsupported_command", &[("command", &text)]);
                Action::None
            }
        }
    }

    /// Render file and local marks, keeping `filter` names if specified.
    fn ex_marks(&self, register: &Register, filter: &str) -> String {
        use std::fmt::Write as _;
        let mut listing = String::from("mark  line  col file/text\n");
        for (&name, &offset) in &self.marks {
            if filter.is_empty() || filter.contains(name) {
                let (line, column) = self.doc.location(offset.min(self.doc.text.len()));
                let _ = writeln!(
                    listing,
                    " {name}   {:>5} {column:>4} {}",
                    line + 1,
                    self.doc.line(line).trim()
                );
            }
        }
        for (name, mark) in register.file_marks.list() {
            if filter.is_empty() || filter.contains(name) {
                let (line, column) = mark.location;
                let file = mark.path.as_ref().map_or_else(
                    || i18n::text("editor.untitled").into(),
                    |path| path.display().to_string(),
                );
                let _ = writeln!(listing, " {name}   {:>5} {column:>4} {file}", line + 1);
            }
        }
        listing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Prefill Ex addresses for a count and a linewise Visual selection, and apply both ranges.
    #[test]
    fn counted_and_visual_command_lines_address_substitution() {
        let mut vim = Vim::new(Document::new("row\nrow\nrow\nrow\n"));
        let mut register = Register::default();
        vim.text("2:", &mut register);
        assert_eq!(vim.command, ".,.+1");
        vim.text("s/row/front/", &mut register);
        vim.key("Enter", &mut register);
        vim.text("jVj:", &mut register);
        assert_eq!(vim.command, "'<,'>");
        vim.text("s/row/tail/", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.text, "front\nfront\ntail\ntail\n");
    }

    /// Preserve the optional filename after either form of a split command.
    #[test]
    fn split_commands_keep_axis_and_filename() {
        let mut vim = Vim::new(Document::new(""));
        for name in ["sp", "split"] {
            assert_eq!(
                vim.ex(name, None, &Register::default()),
                Action::Split {
                    stacked: true,
                    file: None
                },
            );
        }
        for name in ["vs", "vsplit"] {
            assert_eq!(
                vim.ex(
                    &format!("{name}  another file.txt"),
                    None,
                    &Register::default()
                ),
                Action::Split {
                    stacked: false,
                    file: Some("another file.txt".into())
                },
            );
        }
    }
}
