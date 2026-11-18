//! Track Ex command origins and route diagnostics at the active verbosity.

use super::InputMappings;
use crate::{i18n, vim::VimOptions};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

/// Source file and one-based physical line that supplied an Ex command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExOrigin {
    file: String,
    line: usize,
}

impl ExOrigin {
    pub(in crate::vim) fn new(file: &str, line: usize) -> Self {
        Self {
            file: file.into(),
            line,
        }
    }

    /// Physical source line corresponding to the executed Ex command.
    pub(in crate::vim) fn line(&self) -> usize {
        self.line
    }

    /// Attach the original definition location to a verbose listing entry.
    pub(in crate::vim) fn append_to(&self, output: &mut String) {
        self.append_with_separator(output, "\n\t");
    }

    /// Attach the source to an Ex result displayed in the one-row editor footer.
    pub(in crate::vim) fn append_inline(&self, output: &mut String) {
        self.append_with_separator(output, "  ");
    }

    fn append_with_separator(&self, output: &mut String, separator: &str) {
        use std::fmt::Write as _;
        let source = i18n::format(
            "vim_verbose.last_set",
            &[("file", &self.file), ("line", &self.line.to_string())],
        );
        let _ = write!(output, "{separator}{source}");
    }
}

/// The current script statement remains the definition source until dispatch returns.
pub struct ExOriginScope(InputMappings);

impl Drop for ExOriginScope {
    fn drop(&mut self) {
        self.0.0.write().unwrap().verbose.origins.pop();
    }
}

#[derive(Default)]
pub(super) struct State {
    levels: Vec<u32>,
    origins: Vec<Option<ExOrigin>>,
    option_origins: BTreeMap<&'static str, ExOrigin>,
    log_directory: PathBuf,
    log_setting: String,
    log_path: PathBuf,
    log_started: bool,
    log_failed: bool,
    last_diagnostic: String,
}

impl InputMappings {
    /// Override 'verbose' for a single Ex command and its nested dispatch.
    pub fn begin_ex_verbose(&self, level: u32) {
        self.0.write().unwrap().verbose.levels.push(level);
    }

    /// Restore the previous explicit or global verbosity after dispatch.
    pub fn end_ex_verbose(&self) {
        self.0.write().unwrap().verbose.levels.pop();
    }

    /// Resolve the temporary modifier before the global 'verbose' option.
    pub fn ex_verbose(&self) -> u32 {
        let input = self.0.read().unwrap();
        input
            .verbose
            .levels
            .last()
            .copied()
            .unwrap_or(input.options.verbose)
    }

    /// Apply :set's effective value while keeping a modifier-local assignment temporary.
    pub(in crate::vim) fn set_ex_options(&self, mut options: VimOptions) {
        let mut input = self.0.write().unwrap();
        let saved = input.options.verbose;
        if let Some(level) = input.verbose.levels.last_mut() {
            *level = options.verbose;
            options.verbose = saved;
        }
        if options.verbosefile != input.options.verbosefile {
            input.verbose.log_setting.clear();
            input.verbose.log_started = false;
            input.verbose.log_failed = false;
        }
        input.options = options;
    }

    /// Resolve relative verbose log filenames against the current Ex working directory.
    pub fn set_ex_log_directory(&self, directory: &Path) {
        let mut input = self.0.write().unwrap();
        if input.verbose.log_directory != directory {
            input.verbose.log_directory = directory.to_owned();
        }
    }

    /// Retain the current script statement's source through a nested Ex command.
    pub fn enter_ex_origin(&self, origin: Option<ExOrigin>) -> ExOriginScope {
        self.0.write().unwrap().verbose.origins.push(origin);
        ExOriginScope(self.clone())
    }

    pub(in crate::vim) fn current_ex_origin(&self) -> Option<ExOrigin> {
        self.0
            .read()
            .unwrap()
            .verbose
            .origins
            .last()
            .cloned()
            .flatten()
    }

    pub(in crate::vim) fn ex_option_origin(&self, name: &str) -> Option<ExOrigin> {
        self.0
            .read()
            .unwrap()
            .verbose
            .option_origins
            .get(name)
            .cloned()
    }

    pub(in crate::vim) fn set_ex_option_origin(
        &self,
        name: &'static str,
        origin: Option<ExOrigin>,
    ) {
        let mut input = self.0.write().unwrap();
        if name == "verbose" && !input.verbose.levels.is_empty() {
            return;
        }
        match origin {
            Some(origin) => input.verbose.option_origins.insert(name, origin),
            None => input.verbose.option_origins.remove(name),
        };
    }

    /// Append the Ex message stream with the same new-line boundaries as :redir.
    pub fn append_ex_verbosefile(&self, text: &str, newline: bool) {
        if text.is_empty() {
            return;
        }
        let mut input = self.0.write().unwrap();
        let path = input.options.verbosefile.clone();
        if path.is_empty() {
            return;
        }
        let state = &mut input.verbose;
        if state.log_setting != path {
            state.log_path = state.log_directory.join(&path);
            state.log_setting = path;
            state.log_started = false;
            state.log_failed = false;
        }
        if state.log_failed {
            return;
        }
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&state.log_path)
            .and_then(|mut file| {
                if newline && (state.log_started || file.metadata()?.len() > 0) {
                    file.write_all(b"\n")?;
                }
                file.write_all(text.as_bytes())
            });
        state.log_started = true;
        if let Err(error) = result {
            state.log_failed = true;
            let error = i18n::format(
                "vim_verbose.log_failed",
                &[
                    ("file", &state.log_path.display()),
                    ("error", &error.to_string()),
                ],
            );
            state.last_diagnostic.clone_from(&error);
            input.ex_error.clone_from(&error);
            if input.messages.len() == 500 {
                input.messages.pop_front();
            }
            input.messages.push_back(crate::vim::ExMessageOutput {
                text: error,
                spans: Vec::new(),
            });
        }
    }

    /// Emit a diagnostic that is redirected regardless of whether it appears on-screen.
    pub fn trace_ex(&self, level: u32, text: &str) {
        if self.ex_verbose() < level {
            return;
        }
        let text = self.filter_ex_output(text);
        if text.is_empty() {
            return;
        }
        self.redirect_ex_output(&text, true);
        if self.with_options(|options| options.verbosefile.is_empty()) {
            if self.ex_silence().is_none() {
                self.remember_ex_message(&text);
                self.0.write().unwrap().verbose.last_diagnostic = text;
            }
        } else {
            self.append_ex_verbosefile(&text, true);
        }
    }

    /// Preserve the most recent visible diagnostic until the next Ex command.
    pub fn last_ex_diagnostic(&self) -> String {
        self.0.read().unwrap().verbose.last_diagnostic.clone()
    }

    /// Start a new Ex command's diagnostic presentation.
    pub(in crate::vim) fn clear_ex_diagnostic(&self) {
        self.0.write().unwrap().verbose.last_diagnostic.clear();
    }

    /// Trace typed or sourced Ex input at the documented levels.
    pub(in crate::vim) fn trace_ex_line(&self, command: &str) {
        self.trace_ex_line_from(command, self.current_ex_origin().as_ref());
    }

    /// Also accept the exact location of a statement consumed by the script interpreter.
    pub(in crate::vim) fn trace_ex_line_from(&self, command: &str, source: Option<&ExOrigin>) {
        let level = self.ex_verbose();
        if level < 15 || command.is_empty() {
            return;
        }
        if source.is_none() && level < 16 {
            return;
        }
        let end = command.floor_char_boundary(command.len().min(200));
        let command = &command[..end];
        let line = source.map_or_else(
            || format!(":{command}"),
            |origin| format!("line {}: {command}", origin.line()),
        );
        self.trace_ex(if source.is_some() { 15 } else { 16 }, &line);
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        document::Document,
        vim::{Action, Register, ScriptProgram, Vim},
    };

    /// Retrieve local or deferred listing output after executing the Ex modifier.
    fn modified(vim: &mut Vim, register: &mut Register, line: &str) -> Action {
        match vim.execute_ex_line(line, register) {
            Action::ExState { action, .. } => *action,
            action => action,
        }
    }

    /// Counts override global verbosity, including zero, and assignments remain scoped.
    #[test]
    fn verbose_counts_and_runtime_option_restore_after_nested_commands() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("set vbs=4", &mut register);
        modified(&mut vim, &mut register, "verbose set verbose?");
        assert_eq!(vim.message, "verbose=1");
        modified(&mut vim, &mut register, "0verb set vbs");
        assert_eq!(vim.message, "verbose=0");
        modified(&mut vim, &mut register, "6verbose echo &g:verbose");
        assert_eq!(vim.message, "6");
        modified(&mut vim, &mut register, "7verbose set vbs=9");
        vim.execute_ex_line("set vbs?", &mut register);
        assert_eq!(vim.message, "verbose=4");
        vim.execute_ex_line("set vbs&", &mut register);
        assert_eq!(register.input_maps.ex_verbose(), 0);
    }

    /// Provenance is the original source declaration, including modifications inside a function.
    #[test]
    fn verbose_listings_use_real_source_lines() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        let script = concat!(
            "setlocal ts=3\n",
            "nnoremap K <Nop>\n",
            "command Greet echo 'hello'\n",
            "autocmd User Test echo 'event'\n",
            "function SetIndent()\n",
            "  setlocal sw=2\n",
            "endfunction\n",
        );
        let mut program =
            ScriptProgram::compile(vim.script_lines(script, 30), Some("settings.vim".into()))
                .unwrap();
        while let Some(line) = program.next(&vim, &mut register, None).unwrap() {
            vim.execute_script_statement(&program, &line, &mut register);
            assert!(!vim.ex_failed(), "{}", vim.message);
        }
        vim.execute_ex_line("call SetIndent()", &mut register);
        for (command, source_line) in [
            ("set ts?", 30),
            ("map K", 31),
            ("command Greet", 32),
            ("autocmd User Test", 33),
            ("function SetIndent", 34),
            ("set sw?", 35),
        ] {
            let action = modified(&mut vim, &mut register, &format!("verbose {command}"));
            let result = match action {
                Action::ShowListing(text) => text,
                Action::None => vim.message.clone(),
                _ => panic!("Unexpected output for {command}"),
            };
            assert!(result.contains("settings.vim"), "{command}: {result}");
            assert!(
                result.contains(&source_line.to_string()),
                "{command}: {result}"
            );
        }
        vim.execute_ex_line("setlocal ts=8", &mut register);
        modified(&mut vim, &mut register, "verbose set tabstop?");
        assert_eq!(vim.message, "tabstop=8");
        modified(&mut vim, &mut register, "0verbose map K");
        assert!(!vim.message.contains("settings.vim"));
    }

    /// Ordinary Ex messages and scoped traces reach the append-only local log.
    #[test]
    fn verbosefile_logs_messages_and_hides_traces_from_the_footer() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("debug log.txt");
        std::fs::write(&log, "existing").unwrap();
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        register.input_maps.set_ex_log_directory(directory.path());
        vim.execute_ex_line(r"set vfile=debug\ log.txt", &mut register);
        vim.execute_ex_line("echo 'ordinary'", &mut register);
        modified(&mut vim, &mut register, "16verbose echo 'traced'");
        assert_eq!(vim.message, "traced");
        assert!(register.input_maps.last_ex_diagnostic().is_empty());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("existing\nordinary"));
        assert!(text.contains("\n:echo 'traced'\ntraced"));
        vim.execute_ex_line("set verbosefile=", &mut register);
        vim.execute_ex_line("echo 'outside'", &mut register);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), text);
    }
}
