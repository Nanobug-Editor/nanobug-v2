//! Parse Ex operations for views, editor modes and application information.

use super::{Action, Mode, Register, Vim};
use std::{fmt::Write as _, time::Duration};

/// Native names available to Ex command completion.
pub(super) const NAMES: &[&str] = &[
    "colorscheme",
    "language",
    "pbuffer",
    "redraw",
    "redrawstatus",
    "redrawtabline",
    "resize",
    "syncbind",
    "tabrewind",
    "z",
    "quitall",
    "sleep",
    "startgreplace",
    "startinsert",
    "startreplace",
    "stopinsert",
    "xit",
    "exusage",
    "helpclose",
    "intro",
    "options",
    "version",
    "viusage",
    "loadkeymap",
    "menutranslate",
];

/// A viewport dimension requested by :resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewSize {
    /// Make the addressed window as large as the layout permits.
    Maximum,
    /// Size in grid rows or columns.
    Absolute(usize),
    /// Offset from the window's current size.
    Relative(isize),
}

/// Change to an editing mode after the current Ex command completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExModeChange {
    /// Insert text before the current caret.
    Insert,
    /// Replace characters.
    Replace,
    /// Replace display cells, including the width of tab and wide characters.
    VirtualReplace,
    /// End the active insertion or terminal-input session.
    StopInsert,
}

/// A view or native window action to apply outside the Vim input model.
#[derive(Debug, PartialEq, Eq)]
pub enum WindowCommand {
    /// Report or change the active native palette.
    ColorScheme(String),
    /// Report or change Nanobug's UI and message locale.
    Language(String),
    /// Retain a shared buffer in the current Tabpage's preview without changing focus.
    PreviewBuffer {
        selector: String,
        command: Option<String>,
    },
    /// Refresh requested native surfaces.
    Redraw,
    /// Align the relative positions of the active Tabpage's bound views.
    SyncBind,
    /// Resize the addressed pane by rows, or by columns with :vertical.
    Resize {
        window: Option<usize>,
        columns: bool,
        size: ViewSize,
    },
    /// Pause input for `duration`, optionally hiding the caret.
    Sleep {
        duration: Duration,
        hide_caret: bool,
    },
    /// Start or finish the selected mode in the active editor or terminal.
    ChangeMode { kind: ExModeChange, line_end: bool },
    /// Open the generated Ex index, keyboard guide, options or introduction.
    Information(ViewInformation),
    /// Close the current Tabpage's Help window.
    HelpClose,
    /// Evaluate one setting line from the options window in its source pane.
    ApplyOption { pane: usize, command: String },
}

/// Content requested by an Ex command that opens a reference view.
#[derive(Debug, PartialEq, Eq)]
pub enum ViewInformation {
    /// List supported Ex command names.
    ExUsage(String),
    /// Show the interactive keyboard guide.
    ViUsage,
    /// Show the editor's brief opening text.
    Intro,
    /// Show the application version and target.
    Version,
    /// Open the supported options with their current values.
    Options(String),
    /// Describe the supported option selected in the options window.
    OptionHelp(String),
}

impl Vim {
    /// Parse the window and information Ex commands in `text`.
    pub(super) fn ex_window_command(
        &mut self,
        text: &str,
        register: &mut Register,
    ) -> Option<Action> {
        let text = text.trim();
        let (columns, text) = text
            .split_once(char::is_whitespace)
            .filter(|(word, _)| word.len() >= 4 && "vertical".starts_with(word))
            .map_or((false, text), |(_, rest)| (true, rest.trim_start()));
        let first = text.bytes().take_while(u8::is_ascii_digit).count();
        let direct = ex_parts(text[first..].trim_start());
        let (count, name, bang, tail) = if name_is(direct.0, "pbuffer", 2)
            || name_is(direct.0, "resize", 3)
            || name_is(direct.0, "sleep", 2)
        {
            (
                text[..first].parse::<usize>().ok(),
                direct.0,
                direct.1,
                direct.2,
            )
        } else {
            let (range, body) = self.ex_range(text)?;
            let (name, bang, tail) = ex_parts(body);
            if name == "z" && !columns {
                return Some(self.ex_z(range.map(|range| range.end - 1), tail, bang, register));
            }
            if range.is_some() || columns {
                return None;
            }
            (None, name, bang, tail)
        };
        if columns && !name_is(name, "resize", 3) {
            return None;
        }
        let request = if name_is(name, "pbuffer", 2) {
            let (command, selector) = if let Some(rest) = tail.strip_prefix('+') {
                let (command, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                (
                    Some(if command.is_empty() {
                        "$".to_owned()
                    } else {
                        command.to_owned()
                    }),
                    rest.trim(),
                )
            } else {
                (None, tail)
            };
            let selector = count.map_or_else(|| selector.to_owned(), |value| value.to_string());
            WindowCommand::PreviewBuffer { selector, command }
        } else if name_is(name, "resize", 3) && !bang {
            match view_size(tail) {
                Some(size) => WindowCommand::Resize {
                    window: count,
                    columns,
                    size,
                },
                None => return Some(self.invalid_window_command(text)),
            }
        } else if name_is(name, "sleep", 2) {
            match sleep_duration(tail, count) {
                Some(duration) => WindowCommand::Sleep {
                    duration,
                    hide_caret: bang,
                },
                None => return Some(self.invalid_window_command(text)),
            }
        } else if name_is(name, "menutranslate", 5) && !bang {
            return Some(self.translate_ex_menu(tail, register));
        } else if name_is(name, "loadkeymap", 5) && !bang && tail.is_empty() {
            self.ex_failed = true;
            self.message = crate::i18n::text("vim_mapping.loadkeymap_source").into();
            return Some(Action::None);
        } else if tail.is_empty()
            && (name_is(name, "redraw", 4)
                || name_is(name, "redrawstatus", 7)
                || name_is(name, "redrawtabline", 7))
        {
            WindowCommand::Redraw
        } else if !bang && name_is(name, "colorscheme", 4) {
            WindowCommand::ColorScheme(tail.into())
        } else if !bang && name_is(name, "language", 3) {
            WindowCommand::Language(tail.into())
        } else if tail.is_empty() && !bang && name_is(name, "syncbind", 4) {
            WindowCommand::SyncBind
        } else if tail.is_empty() && name == "quitall" {
            return Some(Action::QuitAll(bang));
        } else if tail.is_empty() && name_is(name, "tabrewind", 4) && !bang {
            return Some(Action::TabNavigate(1, Some(1)));
        } else if name_is(name, "xit", 1) {
            return Some(if self.read_only {
                self.reject_edit()
            } else if !self.doc.modified() {
                Action::Close(bang)
            } else if !bang && tail.is_empty() {
                Action::SaveClose
            } else {
                Action::WriteFile {
                    name: (!tail.is_empty()).then(|| tail.to_owned()),
                    force: bang,
                    close: true,
                }
            });
        } else if tail.is_empty() && name_is(name, "startinsert", 4) {
            WindowCommand::ChangeMode {
                kind: ExModeChange::Insert,
                line_end: bang,
            }
        } else if tail.is_empty() && name_is(name, "startreplace", 6) {
            WindowCommand::ChangeMode {
                kind: ExModeChange::Replace,
                line_end: bang,
            }
        } else if tail.is_empty() && name_is(name, "startgreplace", 6) {
            WindowCommand::ChangeMode {
                kind: ExModeChange::VirtualReplace,
                line_end: bang,
            }
        } else if tail.is_empty() && name_is(name, "stopinsert", 5) && !bang {
            WindowCommand::ChangeMode {
                kind: ExModeChange::StopInsert,
                line_end: false,
            }
        } else {
            if bang || !tail.is_empty() {
                return None;
            }
            let info = if name_is(name, "exusage", 3) {
                let (_, commands) = self.command_candidates("", register).unwrap_or_default();
                ViewInformation::ExUsage(format!(
                    "Nanobug Ex commands\n\n{}\n",
                    commands
                        .into_iter()
                        .map(|command| format!(":{command}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
            } else if name_is(name, "viusage", 3) {
                ViewInformation::ViUsage
            } else if name == "intro" {
                ViewInformation::Intro
            } else if name_is(name, "version", 2) {
                ViewInformation::Version
            } else if name_is(name, "options", 3) {
                ViewInformation::Options(self.options_document(register))
            } else if name_is(name, "helpclose", 5) {
                return Some(Action::WindowExtras(WindowCommand::HelpClose));
            } else {
                return None;
            };
            WindowCommand::Information(info)
        };
        Some(Action::WindowExtras(request))
    }

    /// Start a requested mode in this view once its Ex command has completed.
    pub fn apply_ex_mode(
        &mut self,
        kind: ExModeChange,
        line_end: bool,
        register: &mut Register,
    ) -> Action {
        if kind == ExModeChange::StopInsert {
            return if self.mode.is_insert() {
                self.key("Escape", register)
            } else {
                Action::None
            };
        }
        if self.read_only {
            return self.reject_edit();
        }
        if self.mode.is_insert() {
            return Action::None;
        }
        if self.g.ex_mode {
            self.g.ex_mode = false;
        }
        if line_end {
            let row = self.doc.location(self.doc.cursor).0;
            self.doc.cursor = self.doc.line_range(row).end;
        }
        match kind {
            ExModeChange::Insert => self.begin_insert(Mode::Insert, false, 1, false),
            ExModeChange::Replace => self.begin_insert(Mode::Replace, false, 1, false),
            ExModeChange::VirtualReplace => self.begin_virtual_replace(1),
            ExModeChange::StopInsert => unreachable!(),
        }
        Action::None
    }

    /// Set a lone editor's viewport limit for a resize unsupported by its split tree.
    pub fn set_ex_view_height(&mut self, requested: Option<usize>) {
        self.view.height_limit = None;
        self.view.rows = self.view.available_rows;
        if let Some(rows) = requested {
            self.resize_view_height(rows);
        }
    }

    /// Display the range requested by `:z` and place the caret on its destination line.
    fn ex_z(
        &mut self,
        addressed: Option<usize>,
        tail: &str,
        bang: bool,
        register: &Register,
    ) -> Action {
        let tail = tail.trim();
        let (numbered, tail) = tail
            .strip_prefix('#')
            .map_or((false, tail), |rest| (true, rest));
        let marker = tail
            .chars()
            .next()
            .filter(|ch| matches!(ch, '+' | '-' | '^' | '.' | '='));
        let signs = match marker {
            Some(sign @ ('+' | '-')) => tail.chars().take_while(|&next| next == sign).count(),
            Some(_) => 1,
            None => 0,
        };
        let digits = tail[signs..].trim();
        let rows = if digits.is_empty() {
            self.view
                .rows
                .saturating_sub(if bang { 1 } else { 3 })
                .max(1)
        } else if let Ok(rows @ 1..) = digits.parse::<usize>() {
            rows.saturating_add(usize::from(marker == Some('=')) * 2)
        } else {
            return self.invalid_window_command(tail);
        };
        let current = addressed.unwrap_or_else(|| self.doc.location(self.doc.cursor).0);
        let last = self.line_count().saturating_sub(1);
        let current_row = current as i128;
        let rows = rows.min(2 * self.line_count() + 2) as i128;
        let (start, end, new) = match marker {
            Some('-') => {
                let start = current_row - rows * signs as i128 + 1;
                let end = start + rows - 1;
                (start, end, end)
            }
            Some('^') => (
                current_row - rows * 2,
                current_row - rows,
                current_row - rows,
            ),
            Some('.' | '=') => {
                let half = (rows + 1) / 2 - 1;
                let end = current_row + half;
                (
                    current_row - half,
                    end,
                    if marker == Some('=') {
                        current_row
                    } else {
                        end
                    },
                )
            }
            _ => {
                let start = if marker == Some('+') {
                    current_row + rows * (signs - 1) as i128 + 1
                } else {
                    current_row + i128::from(addressed.is_none())
                };
                let end = start + rows - 1;
                (start, end, end)
            }
        };
        let numbers = numbered || self.view_options(register).number;
        let mut text = String::new();
        let divider =
            (marker == Some('=')).then(|| "-".repeat(self.view.columns.saturating_sub(1)));
        for row in start.max(0)..=end.min(last as i128) {
            let row = row as usize;
            if let Some(divider) = &divider
                && row == current
            {
                let _ = writeln!(text, "{divider}");
            }
            if numbers {
                let _ = write!(text, "{:>6}  ", row + 1);
            }
            let _ = writeln!(text, "{}", self.doc.line(row));
            if let Some(divider) = &divider
                && row == current
            {
                let _ = writeln!(text, "{divider}");
            }
        }
        self.set_cursor(self.first_nonblank(new.clamp(0, last as i128) as usize));
        if text.is_empty() {
            Action::None
        } else {
            Action::ShowListing(text)
        }
    }

    /// Create the scratch Ex options window with editable setting lines.
    fn options_document(&mut self, register: &mut Register) -> String {
        let previous = std::mem::take(&mut self.message);
        self.configure_options("set all", register);
        let values = std::mem::replace(&mut self.message, previous);
        let mut output = String::from(
            "Nanobug options\n\nEdit a set line and press Enter to apply it.\nPress Enter on an option name to show its meaning.\n\n",
        );
        for value in values.split_whitespace() {
            let name = value.split_once('=').map_or_else(
                || value.strip_prefix("no").unwrap_or(value),
                |(name, _)| name,
            );
            if let Some(help) = option_help(name) {
                let _ = writeln!(output, "{name}\t{help}");
                let _ = writeln!(output, "  set {value}");
            }
        }
        output
    }

    /// Route Enter in the options scratch window to help or its source pane.
    pub(super) fn ex_options_enter(&self) -> Option<Action> {
        let pane = self.options_editor?;
        let row = self.doc.location(self.doc.cursor).0;
        let line = self.doc.line(row).trim();
        if line.starts_with("set ") {
            return Some(Action::WindowExtras(WindowCommand::ApplyOption {
                pane,
                command: line.to_owned(),
            }));
        }
        let (name, _) = line.split_once('\t')?;
        let help = option_help(name)?;
        Some(Action::WindowExtras(WindowCommand::Information(
            ViewInformation::OptionHelp(format!(
                "{name}\n\n{help}\n\n:set {name}? shows its current value.\n"
            )),
        )))
    }

    /// Route this scratch options window to the editor with stable `pane` identity.
    pub fn mark_ex_options_editor(&mut self, pane: usize) {
        self.options_editor = Some(pane);
    }

    /// Keep option preview edits from triggering file saving or close confirmation.
    pub fn acknowledge_ex_options_editor_edit(&mut self) {
        if self.options_editor.is_some() {
            self.doc.acknowledge_scratch_edit();
        }
    }

    /// Report an invalid argument for an otherwise recognized Ex command.
    fn invalid_window_command(&mut self, argument: &str) -> Action {
        self.ex_failed = true;
        self.message =
            crate::i18n::format("editor.invalid_ex_argument", &[("argument", &argument)]);
        Action::None
    }
}

/// Split the command name, its immediate bang and its arguments.
fn ex_parts(text: &str) -> (&str, bool, &str) {
    let end = text.bytes().take_while(u8::is_ascii_alphabetic).count();
    let (name, rest) = text.split_at(end);
    let (bang, rest) = rest
        .strip_prefix('!')
        .map_or((false, rest), |rest| (true, rest));
    (name, bang, rest.trim_start())
}

/// Match an Ex name with a documented minimum abbreviation.
fn name_is(actual: &str, canonical: &str, minimum: usize) -> bool {
    actual.len() >= minimum && canonical.starts_with(actual)
}

/// Decode a maximum, absolute or signed resize amount.
fn view_size(value: &str) -> Option<ViewSize> {
    match value {
        "" => Some(ViewSize::Maximum),
        "+" => Some(ViewSize::Relative(1)),
        "-" => Some(ViewSize::Relative(-1)),
        text if text.starts_with(['+', '-']) => text.parse().ok().map(ViewSize::Relative),
        text => text.parse().ok().map(ViewSize::Absolute),
    }
}

/// Decode seconds by default or milliseconds with the native `m` suffix.
fn sleep_duration(value: &str, count: Option<usize>) -> Option<Duration> {
    let millis = value.ends_with('m');
    let value = value.strip_suffix('m').unwrap_or(value).trim();
    let amount = if value.is_empty() {
        count.unwrap_or(1) as u64
    } else {
        value.parse().ok()?
    };
    Some(if millis {
        Duration::from_millis(amount)
    } else {
        Duration::from_secs(amount)
    })
}

/// Describe an option implemented by Nanobug's Ex settings model.
fn option_help(name: &str) -> Option<&'static str> {
    Some(match name {
        "number" => {
            "Display the absolute number of the current line, or all absolute line numbers when relative numbering is off."
        }
        "relativenumber" => "Display each line's distance from the cursor line.",
        "wrap" => "Display long lines across multiple screen rows in this window.",
        "scrollbind" => "Scroll this window together with other bound windows on the Tabpage.",
        "tabstop" => "Set the number of columns occupied by a tab stop.",
        "shiftwidth" => "Set the number of columns used by an indentation step.",
        "expandtab" => "Insert spaces instead of a tab character in Insert mode.",
        "autoindent" => "Copy the indentation of the current line when opening a new line.",
        "ignorecase" => "Ignore case when matching search patterns.",
        "smartcase" => "Match case when a search pattern contains uppercase characters.",
        "hlsearch" => "Highlight all matches of the last accepted search.",
        "incsearch" => "Preview matches while entering a search pattern.",
        "scrolloff" => "Keep a minimum number of screen rows around the cursor when possible.",
        "timeout" => "Time out an incomplete mapped key sequence.",
        "timeoutlen" => "Wait this many milliseconds for a mapped key sequence to complete.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Window aliases, addresses and milliseconds become the same native actions as other routes.
    #[test]
    fn parses_native_window_and_process_arguments() {
        let mut vim = Vim::new(Document::new("one\ntwo\nthree"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("quitall!", &mut register),
            Action::QuitAll(true)
        );
        assert_eq!(
            vim.execute_ex_line("tabrewind", &mut register),
            Action::TabNavigate(1, Some(1))
        );
        assert_eq!(
            vim.execute_ex_line("vertical 2resize -3", &mut register),
            Action::WindowExtras(WindowCommand::Resize {
                window: Some(2),
                columns: true,
                size: ViewSize::Relative(-3)
            })
        );
        assert_eq!(
            vim.execute_ex_line("2pbuffer +3", &mut register),
            Action::WindowExtras(WindowCommand::PreviewBuffer {
                selector: "2".into(),
                command: Some("3".into())
            })
        );
        assert_eq!(
            vim.execute_ex_line("sleep! 18m", &mut register),
            Action::WindowExtras(WindowCommand::Sleep {
                duration: Duration::from_millis(18),
                hide_caret: true
            })
        );
    }

    /// Bang on an insert command begins at the actual line end, then StopInsert returns to Normal.
    #[test]
    fn appends_and_replaces_at_the_requested_position() {
        let mut vim = Vim::new(Document::new("text\nsecond"));
        let mut register = Register::default();
        vim.apply_ex_mode(ExModeChange::Insert, true, &mut register);
        vim.text("!", &mut register);
        vim.apply_ex_mode(ExModeChange::StopInsert, false, &mut register);
        assert_eq!(vim.doc.text, "text!\nsecond");
        assert_eq!(vim.mode, Mode::Normal);
        vim.set_cursor(0);
        vim.apply_ex_mode(ExModeChange::VirtualReplace, false, &mut register);
        vim.text("X", &mut register);
        vim.apply_ex_mode(ExModeChange::StopInsert, false, &mut register);
        assert_eq!(vim.doc.text, "Xext!\nsecond");
    }

    /// Numbered :z output advances to the last printed line without editing text.
    #[test]
    fn prints_z_ranges_and_moves_the_caret() {
        let mut vim = Vim::new(Document::new("one\ntwo\nthree\nfour"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("2z#2", &mut register),
            Action::ShowListing("     2  two\n     3  three\n".into())
        );
        assert_eq!(vim.doc.cursor, vim.doc.starts[2]);
        assert!(!vim.doc.modified());
    }

    /// Menu translations apply component by component without losing original command paths.
    #[test]
    fn translated_menu_can_be_invoked_by_its_original_name() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("menutranslate &FILE &Datei", &mut register);
        vim.execute_ex_line("menutranslate Open\\ File Öffnen", &mut register);
        vim.execute_ex_line("nmenu &File.Open\\ File ihello<Esc>", &mut register);
        assert_eq!(vim.menu_entries(&register, &[])[0].label, "Datei");
        assert_eq!(
            vim.menu_entries(&register, &["File".into()])[0].label,
            "Öffnen"
        );
        vim.execute_ex_line("emenu File.Open\\ File", &mut register);
        assert_eq!(vim.doc.text, "hello");
        vim.execute_ex_line("menutranslate clear", &mut register);
        vim.execute_ex_line("nmenu Popup.Open\\ File iother<Esc>", &mut register);
        assert_eq!(
            vim.menu_entries(&register, &["Popup".into()])[0].label,
            "Open File"
        );
    }

    /// Loading a keymap table directly affects only the buffer and preserves the short name.
    #[test]
    fn sourced_keymap_defines_buffer_local_language_keys() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        assert!(vim.load_ex_keymap("let b:keymap_name = 'local'\nloadkeymap\na あ\n"));
        vim.execute_ex_line("set iminsert=1", &mut register);
        vim.key("i", &mut register);
        vim.text("a", &mut register);
        vim.key("Escape", &mut register);
        assert_eq!(vim.doc.text, "あ");
        let mut other = Vim::new(Document::new(""));
        other.execute_ex_line("set iminsert=1", &mut register);
        other.key("i", &mut register);
        other.text("a", &mut register);
        assert_eq!(other.doc.text, "a");
    }
}
