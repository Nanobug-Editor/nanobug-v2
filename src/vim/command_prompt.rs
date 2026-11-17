//! Candidate and history navigation for Ex, search and shell filter prompts.

use super::{Mode, Register, Vim, commands::ExCommand};
use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

/// Maximum commands to retain in each history, matching Neovim's default.
const HISTORY_LIMIT: usize = 10_000;

/// Accepted prompt entries shared by the buffers in one editor pane.
#[derive(Default)]
pub(super) struct Histories {
    /// Entries grouped by Ex, search, shell filter, or expression, oldest first.
    entries: [Vec<String>; 4],
}

impl Histories {
    /// Format the requested history, with all prompt groups for `all`.
    pub(super) fn ex_listing(&self, filter: &str) -> String {
        use std::fmt::Write as _;
        let groups = match filter {
            "" | ":" | "cmd" => &[(0, "Command")][..],
            "/" | "?" | "search" => &[(1, "Search")][..],
            "!" | "input" => &[(2, "Shell")][..],
            "=" | "expr" => &[(3, "Expression")][..],
            "all" => &[
                (0, "Command"),
                (1, "Search"),
                (2, "Shell"),
                (3, "Expression"),
            ][..],
            _ => return format!("Unknown history: {filter}"),
        };
        let mut listing = String::new();
        for &(index, title) in groups {
            let _ = writeln!(listing, "{title} history\n      #  text");
            for (number, text) in self.entries[index].iter().enumerate() {
                let _ = writeln!(listing, "{:>7}  {text}", number + 1);
            }
        }
        listing
    }

    /// Read the most recently accepted line in `prompt`'s history.
    pub(super) fn latest(&self, prompt: char) -> Option<&str> {
        self.entries[history_index(prompt)]
            .last()
            .map(String::as_str)
    }

    /// Record the submitted `text` in its `prompt` history, newest entries winning duplicates.
    pub(super) fn remember(&mut self, prompt: char, text: &str) {
        if text.is_empty() {
            return;
        }
        let entries = &mut self.entries[history_index(prompt)];
        if let Some(index) = entries.iter().position(|prior| prior == text) {
            entries.remove(index);
        } else if entries.len() == HISTORY_LIMIT {
            entries.remove(0);
        }
        entries.push(text.into());
    }
}

/// Choose the history shared by the two search directions or used by `prompt` alone.
fn history_index(prompt: char) -> usize {
    match prompt {
        '/' | '?' => 1,
        '!' => 2,
        '=' => 3,
        _ => 0,
    }
}

/// Input state specific to the current command-line prompt.
#[derive(Default)]
pub(super) struct State {
    /// Host directory used by commands that accept a relative path.
    directory: PathBuf,
    /// Candidate cycle for the currently completed text.
    completion: Option<Completion>,
    /// History navigation and the draft to recover on returning to the newest entry.
    history: Option<History>,
    /// Whether typing replaces existing graphemes in the current prompt.
    pub(super) overstrike: bool,
    /// Alternate file in the editor pane, supplied by its native owner.
    pub(super) alternate_path: Option<PathBuf>,
    /// Configured local thesaurus files for Insert-mode synonym completion.
    pub(super) thesaurus: Vec<PathBuf>,
    /// Original source caret before starting the current incremental search.
    pub(super) search_origin: Option<usize>,
    /// Current match preview in document bytes, if the pattern is complete.
    pub(super) search_preview: Option<Range<usize>>,
}

impl State {
    /// Drop navigation for the current text, retaining the host directory.
    pub(super) fn reset(&mut self) {
        self.completion = None;
        self.history = None;
    }

    /// Save the navigation fields of the outer prompt while editing a nested expression.
    pub(super) fn take_navigation(&mut self) -> Self {
        Self {
            completion: self.completion.take(),
            history: self.history.take(),
            overstrike: std::mem::take(&mut self.overstrike),
            search_origin: self.search_origin.take(),
            search_preview: self.search_preview.take(),
            ..Self::default()
        }
    }

    /// Restore the saved navigation fields without discarding current buffer context.
    pub(super) fn restore_navigation(&mut self, saved: Self) {
        self.completion = saved.completion;
        self.history = saved.history;
        self.overstrike = saved.overstrike;
        self.search_origin = saved.search_origin;
        self.search_preview = saved.search_preview;
    }
}

/// The byte range replaced by each successive completion candidate.
struct Completion {
    /// Range occupied by the current candidate in the command.
    span: Range<usize>,
    /// Original text, restored when canceling a candidate cycle.
    original: String,
    /// Sorted matches from the same command context.
    matches: Vec<String>,
    /// Current position within `matches`.
    selected: usize,
}

/// Location in a single history during one Up/Down sequence.
struct History {
    /// Prompt draft saved before the first history movement.
    draft: String,
    /// Byte caret within the draft, also the end of the filter prefix.
    cursor: usize,
    /// Current history position, with the draft just after the last entry.
    selected: usize,
}

impl Vim {
    /// Supply the active pane's alternate buffer `path` for the # register.
    pub fn set_alternate_file(&mut self, path: Option<PathBuf>) {
        self.command_prompt.alternate_path = path;
    }

    /// Return editable history for `prompt` followed by its current draft, with a byte caret.
    pub fn command_window_text(&self, prompt: char, register: &Register) -> (String, usize) {
        let mut text = register.command_history.entries[history_index(prompt)].join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        let mut cursor = text.len();
        if matches!(self.mode, Mode::Command | Mode::Search) && self.prompt == prompt {
            cursor += self.command_cursor();
            text.push_str(&self.command);
        }
        (text, cursor)
    }

    /// Transfer a selected `line` from the history window into `prompt` at byte `cursor`.
    /// Keep the existing search-operator or range context when the window came from that prompt.
    pub fn restore_command_window_line(&mut self, prompt: char, line: String, cursor: usize) {
        if !matches!(self.mode, Mode::Command | Mode::Search) || self.prompt != prompt {
            self.open_prompt(prompt, 1);
        }
        self.command = line;
        self.command_pos = self
            .command
            .floor_char_boundary(cursor.min(self.command.len()));
        self.command_prompt.reset();
        self.preview_search(0);
    }

    /// Use the host's `directory` as the base for the editor's path completion.
    pub fn set_completion_directory(&mut self, directory: &Path) {
        if self.command_prompt.directory != directory {
            self.command_prompt.directory = directory.to_owned();
            self.command_prompt.reset();
        }
    }

    /// Return the configured base for a file completion, falling back to the buffer's directory.
    pub(super) fn completion_directory(&self) -> &Path {
        if self.command_prompt.directory.as_os_str().is_empty() {
            self.doc
                .path
                .as_deref()
                .and_then(Path::parent)
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
        } else {
            &self.command_prompt.directory
        }
    }

    /// Move to the next or previous Ex candidate, retaining text after the caret.
    /// Return whether completion consumed the navigation key.
    pub(super) fn cycle_command_completion(
        &mut self,
        backward: bool,
        start: bool,
        register: &Register,
    ) -> bool {
        if self.mode != Mode::Command || self.prompt != ':' {
            return false;
        }
        if let Some(completion) = &mut self.command_prompt.completion {
            let len = completion.matches.len();
            completion.selected = (completion.selected + if backward { len - 1 } else { 1 }) % len;
            let candidate = &completion.matches[completion.selected];
            self.command
                .replace_range(completion.span.clone(), candidate);
            self.command_pos = completion.span.start + candidate.len();
            completion.span.end = self.command_pos;
            return true;
        }
        if !start {
            return false;
        }
        let caret = self.command_cursor();
        let Some((begin, matches)) = self.command_candidates(&self.command[..caret], register)
        else {
            return true;
        };
        let selected = if backward { matches.len() - 1 } else { 0 };
        let original = self.command[begin..caret].to_owned();
        self.command.replace_range(begin..caret, &matches[selected]);
        self.command_pos = begin + matches[selected].len();
        self.command_prompt.history = None;
        if matches.len() > 1 {
            self.command_prompt.completion = Some(Completion {
                span: begin..self.command_pos,
                original,
                matches,
                selected,
            });
        }
        true
    }

    /// Restore the original command text when Ctrl+e dismisses a completion cycle.
    pub(super) fn cancel_command_completion(&mut self) -> bool {
        let Some(completion) = self.command_prompt.completion.take() else {
            return false;
        };
        self.command_pos = completion.span.start + completion.original.len();
        self.command
            .replace_range(completion.span, &completion.original);
        true
    }

    /// List, insert all, or extend the common prefix of Ex completions using `key`.
    pub(super) fn complete_command_group(&mut self, key: &str, register: &Register) {
        let caret = self.command_cursor();
        if key == "Ctrl+d" {
            let matches = self
                .command_prompt
                .completion
                .as_ref()
                .map(|completion| completion.matches.join("  "))
                .or_else(|| {
                    self.command_candidates(&self.command[..caret], register)
                        .map(|(_, matches)| matches.join("  "))
                });
            self.message = matches.unwrap_or_default();
            return;
        }
        let (span, matches) = if let Some(completion) = self.command_prompt.completion.take() {
            (completion.span, completion.matches)
        } else if let Some((begin, matches)) =
            self.command_candidates(&self.command[..caret], register)
        {
            (begin..caret, matches)
        } else {
            return;
        };
        let value = match key {
            "Ctrl+a" => matches.join(" "),
            _ => {
                let first = &matches[0];
                let mut end = first.len();
                for other in &matches[1..] {
                    end = first[..end]
                        .bytes()
                        .zip(other.bytes())
                        .take_while(|(a, b)| a == b)
                        .count();
                    end = first.floor_char_boundary(end);
                }
                if !first[..end].starts_with(&self.command[span.clone()]) {
                    return;
                }
                first[..end].to_owned()
            }
        };
        self.command_pos = span.start + value.len();
        self.command.replace_range(span, &value);
        self.command_prompt.reset();
    }

    /// Recall an older or newer entry from `register`, optionally filtering by the draft prefix.
    pub(super) fn recall_command(&mut self, register: &Register, older: bool, prefix: bool) {
        self.command_prompt.completion = None;
        let entries = &register.command_history.entries[history_index(self.prompt)];
        if entries.is_empty() {
            return;
        }
        let cursor = self.command_cursor();
        let history = self.command_prompt.history.get_or_insert_with(|| History {
            cursor,
            draft: self.command.clone(),
            selected: entries.len(),
        });
        let matches = |&index: &usize| {
            !prefix || entries[index].starts_with(&history.draft[..history.cursor])
        };
        let next = if older {
            (0..history.selected).rev().find(matches)
        } else {
            (history.selected + 1..entries.len())
                .find(matches)
                .or(Some(entries.len()))
        };
        if let Some(next) = next {
            history.selected = next;
            if let Some(text) = entries.get(next) {
                self.command.clone_from(text);
                self.command_pos = self.command.len();
            } else {
                self.command.clone_from(&history.draft);
                self.command_pos = history.cursor;
            }
        }
    }

    /// Complete the Ex name or its argument before the caret in `head`, retaining a typed range.
    pub(super) fn command_candidates(
        &self,
        head: &str,
        register: &Register,
    ) -> Option<(usize, Vec<String>)> {
        let (_, name_and_args) = self.ex_range(head)?;
        let trimmed = name_and_args.trim_start();
        let leading = head.len() - trimmed.len();
        let (begin, matches) = if let Some(end) = trimmed.find(char::is_whitespace) {
            let (name, remainder) = trimmed.split_at(end);
            let option = remainder.trim_start();
            if matches!(
                name,
                "set" | "se" | "setlocal" | "setl" | "setglobal" | "setg"
            ) {
                let prefix = option
                    .strip_prefix("keymap=")
                    .or_else(|| option.strip_prefix("kmp="));
                if let Some(prefix) = prefix {
                    let candidates = super::keymap::candidates(self.completion_directory(), prefix);
                    return (!candidates.is_empty())
                        .then_some((head.len() - prefix.len(), candidates));
                }
                let fragment = option.split_whitespace().last().unwrap_or_default();
                let option_name = fragment
                    .strip_prefix("no")
                    .or_else(|| fragment.strip_prefix("inv"))
                    .unwrap_or(fragment);
                let options = ["keymap=", "keymap?"]
                    .into_iter()
                    .chain(super::options::candidates(option_name))
                    .filter(|candidate| candidate.starts_with(option_name))
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if !options.is_empty() {
                    return Some((head.len() - option_name.len(), options));
                }
            }
            if let Some((argument, matches)) =
                super::menu::complete_argument(name, remainder, register)
            {
                return (!matches.is_empty()).then_some((head.len() - argument.len(), matches));
            }
            if let Some(completion) = self.user_ex_completion(name, register) {
                use super::script::UserExCompletion;
                let argument = remainder.rsplit(char::is_whitespace).next().unwrap_or("");
                let matches = match completion {
                    UserExCompletion::File | UserExCompletion::Directory => {
                        let mut matches = path_candidates(self.completion_directory(), argument);
                        if matches!(completion, UserExCompletion::Directory) {
                            matches.retain(|name| name.ends_with(std::path::is_separator));
                        }
                        matches
                    }
                    UserExCompletion::Command => self.command_candidates(argument, register)?.1,
                    UserExCompletion::Option => super::options::candidates(argument)
                        .map(str::to_owned)
                        .collect(),
                };
                return (!matches.is_empty()).then_some((head.len() - argument.len(), matches));
            }
            let (command, _) = ExCommand::parse(name)?;
            let path = if command.takes_path() {
                remainder.trim_start()
            } else if matches!(command, ExCommand::Set) {
                remainder.trim_start().strip_prefix("thesaurus=")?
            } else {
                return None;
            };
            (
                head.len() - path.len(),
                path_candidates(self.completion_directory(), path),
            )
        } else {
            let mut candidates = ExCommand::candidates(trimmed);
            candidates.extend(self.user_ex_candidates(trimmed, register));
            candidates.extend(super::mappings::ex_candidates(trimmed).map(str::to_owned));
            candidates.extend(super::menu::ex_candidates(trimmed));
            candidates.extend(super::ex_batch::candidates(trimmed).map(str::to_owned));
            candidates.extend(
                super::ex_files::NAMES
                    .iter()
                    .chain(super::ex_buffer_lifecycle::NAMES)
                    .chain(super::ex_catalog::NAMES)
                    .chain(super::ex_history::NAMES)
                    .chain(super::ex_message_modifiers::NAMES)
                    .chain(super::ex_window_modifiers::NAMES)
                    .chain(super::session_export::NAMES)
                    .chain(super::ex_state::NAMES)
                    .chain(super::ex_windows::NAMES)
                    .chain(super::script::EX_NAMES)
                    .filter(|name| name.starts_with(trimmed))
                    .map(|name| (*name).to_owned()),
            );
            if "nohlsearch".starts_with(trimmed) {
                candidates.push("nohlsearch".into());
            }
            candidates.sort_unstable();
            candidates.dedup();
            (leading, candidates)
        };
        (!matches.is_empty()).then_some((begin, matches))
    }
}

/// List UTF-8 path matches beneath `directory`, preserving any typed directory and separator.
pub(super) fn path_candidates(directory: &Path, typed: &str) -> Vec<String> {
    let split = typed
        .rfind(std::path::is_separator)
        .map_or(0, |index| index + 1);
    let (parent, prefix) = typed.split_at(split);
    let separator = parent.chars().next_back().unwrap_or('/');
    let Ok(entries) = fs::read_dir(directory.join(parent)) else {
        return Vec::new();
    };
    let prefix_lowercase = cfg!(windows).then(|| prefix.to_lowercase());
    let mut candidates: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if (!prefix.starts_with('.') && name.starts_with('.'))
                || !(name.starts_with(prefix)
                    || prefix_lowercase
                        .as_ref()
                        .is_some_and(|prefix| name.to_lowercase().starts_with(prefix)))
            {
                return None;
            }
            let mut candidate = format!("{parent}{name}");
            if entry.path().is_dir() {
                candidate.push(separator);
            }
            Some(candidate)
        })
        .collect();
    candidates.sort_unstable();
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Action};

    /// Create a prompt containing `text` using the same ingress as native IME.
    fn prompt(vim: &mut Vim, register: &mut Register, key: &str, text: &str) {
        vim.key(key, register);
        vim.commit_text(text, register);
    }

    /// Command prompts share the literal parser and let word motions retain an editable draft.
    #[test]
    fn command_prompt_accepts_literal_keys_digraphs_and_overstrike() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        prompt(&mut vim, &mut register, ":", "alpha 世界");
        vim.key("Ctrl+Left", &mut register);
        vim.key("Insert", &mut register);
        vim.key("Ctrl+k", &mut register);
        vim.key("a", &mut register);
        vim.key(":", &mut register);
        assert_eq!(vim.command, "alpha ä界");
        vim.key("Shift+Right", &mut register);
        vim.key("Ctrl+q", &mut register);
        vim.key("Ctrl+c", &mut register);
        assert_eq!(vim.command, "alpha ä界\u{3}");
        assert_eq!(vim.mode, Mode::Command);
    }

    /// Keep an Ex candidate cycle alive when listing it and insert every matching name on request.
    #[test]
    fn command_completion_lists_and_inserts_all_candidates() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        prompt(&mut vim, &mut register, ":", "tab");
        vim.key("Tab", &mut register);
        vim.key("Ctrl+d", &mut register);
        let names = [
            "tab",
            "tabNext",
            "tabclose",
            "tabdo",
            "tabedit",
            "tabfind",
            "tabfirst",
            "tablast",
            "tabmove",
            "tabnew",
            "tabnext",
            "tabonly",
            "tabprevious",
            "tabrewind",
            "tabs",
        ];
        assert_eq!(vim.message, names.join("  "));
        vim.key("Ctrl+n", &mut register);
        assert_eq!(vim.command, "tabNext");
        vim.key("Ctrl+a", &mut register);
        assert_eq!(vim.command, names.join(" "));
    }

    /// Search control keys move among previewed matches, preserve selection on extension and cancel.
    #[test]
    fn incremental_search_cycles_extends_and_restores_the_source() {
        let mut vim = Vim::new(Document::new("x one1 one2 one3"));
        let mut register = Register::default();
        prompt(&mut vim, &mut register, "/", "one");
        assert_eq!(vim.doc.cursor, 2);
        vim.key("Ctrl+g", &mut register);
        assert_eq!(vim.doc.cursor, 7);
        vim.key("Ctrl+l", &mut register);
        assert_eq!(vim.command, "one2");
        assert_eq!(vim.doc.cursor, 7);
        vim.key("Escape", &mut register);
        assert_eq!(vim.doc.cursor, 0);
        prompt(&mut vim, &mut register, "/", "one");
        vim.key("Ctrl+t", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.cursor, 12);
        vim.key("Ctrl+o", &mut register);
        assert_eq!(vim.doc.cursor, 0);
    }

    /// Ex completion cycles through accepted command names in both directions.
    #[test]
    fn command_completion_cycles_only_implemented_names() {
        let mut vim = Vim::new(Document::new("sample"));
        let mut register = Register::default();
        prompt(&mut vim, &mut register, ":", "tabn");
        for (key, want) in [
            ("Tab", "tabnew"),
            ("Tab", "tabnext"),
            ("Tab", "tabnew"),
            ("Shift+Tab", "tabnext"),
            ("Ctrl+e", "tabn"),
            ("Tab", "tabnew"),
        ] {
            assert_eq!(vim.key(key, &mut register), Action::None);
            assert_eq!(vim.command, want);
        }
        assert_eq!(vim.key("Enter", &mut register), Action::TabNew(None));
    }

    /// File completion uses the host directory and preserves whitespace and Unicode in names.
    #[test]
    fn path_completion_uses_the_host_directory() {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join("目录")).unwrap();
        fs::write(project.path().join("目录").join("文 件.txt"), "").unwrap();
        let mut vim = Vim::new(Document::new("sample"));
        let mut register = Register::default();
        vim.set_completion_directory(project.path());
        prompt(&mut vim, &mut register, ":", "e  目");
        vim.key("Tab", &mut register);
        assert_eq!(vim.command, "e  目录/");
        vim.key("Tab", &mut register);
        assert_eq!(vim.command, "e  目录/文 件.txt");
        assert_eq!(
            vim.key("Enter", &mut register),
            Action::Open("目录/文 件.txt".into())
        );
    }

    /// History is shared within a pane, prefix-filtered, and restores text after the draft caret.
    #[test]
    fn prompt_history_preserves_the_draft_and_separates_search() {
        let mut first = Vim::new(Document::new("sample"));
        let mut second = Vim::new(Document::new("sample"));
        let mut register = Register::default();
        for (key, text) in [
            (":", "edit 一号"),
            (":", "quit"),
            (":", "edit 二号"),
            ("/", "sample"),
        ] {
            prompt(&mut first, &mut register, key, text);
            first.key("Enter", &mut register);
        }

        prompt(&mut second, &mut register, ":", "ed览");
        second.key("Left", &mut register);
        for (key, want, caret) in [
            ("Up", "edit 二号", "edit 二号".len()),
            ("Up", "edit 一号", "edit 一号".len()),
            ("Up", "edit 一号", "edit 一号".len()),
            ("Down", "edit 二号", "edit 二号".len()),
            ("Down", "ed览", 2),
        ] {
            assert_eq!(second.key(key, &mut register), Action::None);
            assert_eq!(second.command, want);
            assert_eq!(second.command_cursor(), caret);
        }
        second.key("Escape", &mut register);
        second.key("?", &mut register);
        second.key("Up", &mut register);
        assert_eq!(second.command, "sample");
        second.key("Down", &mut register);
        assert!(second.command.is_empty());
    }

    /// A command-window action leaves the originating prompt and draft available for editing.
    #[test]
    fn command_window_retains_prompt_and_shared_history() {
        let mut vim = Vim::new(Document::new("first cat\nsecond cat\n"));
        let mut register = Register::default();
        prompt(&mut vim, &mut register, ":", "write 中文");
        assert_eq!(
            vim.key("Enter", &mut register),
            Action::WriteFile {
                name: Some("中文".into()),
                force: false,
                close: false
            }
        );
        prompt(&mut vim, &mut register, "/", "cat");
        vim.key("Enter", &mut register);
        for (key, prompt, history) in [
            (":", ':', "write 中文\n"),
            ("/", '/', "cat\n"),
            ("?", '?', "cat\n"),
        ] {
            assert_eq!(vim.key("q", &mut register), Action::None);
            assert!(vim.sequence_hint().is_some());
            assert_eq!(vim.key(key, &mut register), Action::CommandWindow(prompt));
            assert_eq!(
                vim.command_window_text(prompt, &register),
                (history.into(), history.len())
            );
        }
        prompt(&mut vim, &mut register, ":", "edit 目录");
        vim.key("Left", &mut register);
        assert_eq!(vim.key("Ctrl+f", &mut register), Action::CommandWindow(':'));
        assert_eq!(vim.command, "edit 目录");
        assert_eq!(
            vim.command_window_text(':', &register),
            ("write 中文\nedit 目录".into(), "write 中文\nedit 目".len())
        );
        vim.restore_command_window_line(':', "edit 文件".into(), 6);
        assert_eq!(vim.command_cursor(), 5);
        assert_eq!(vim.key("Enter", &mut register), Action::Open("文件".into()));
    }
}
