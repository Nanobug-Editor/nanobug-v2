//! Vim function definitions and the command subset used by custom operators and Help examples.

mod autocmd;
mod control;
mod encoding;
mod user_commands;

use super::{Action, ExOrigin, Mode, Register, Vim, expression::Value};
use crate::i18n;
pub use autocmd::{ScriptAutocmd, ScriptEvent};
pub use control::{ScriptLine, ScriptProgram, ScriptTabScope};
pub use encoding::decode_ex_source;
use std::{collections::BTreeMap, fmt::Write as _, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
pub(super) use user_commands::Completion as UserExCompletion;

/// Internal operator key stored while g@ waits for a motion.
pub(super) const USER_OPERATOR: char = '\u{10}';

/// Script command spellings exposed by Ex completion outside the host table.
pub(super) const EX_NAMES: &[&str] = &[
    "augroup",
    "autocmd",
    "doautoall",
    "doautocmd",
    "noautocmd",
    "command",
    "comclear",
    "delcommand",
    "break",
    "catch",
    "const",
    "continue",
    "defer",
    "else",
    "elseif",
    "endfor",
    "endif",
    "endtry",
    "endwhile",
    "for",
    "finally",
    "if",
    "lockvar",
    "throw",
    "try",
    "unlet",
    "unlockvar",
    "while",
    "delfunction",
    "echoerr",
    "echon",
    "eval",
    "finish",
    "scriptnames",
    "scriptencoding",
];

/// Script state shared across the buffers of one editor pane.
#[derive(Default)]
pub(super) struct State {
    /// Block nested g== evaluations from Help code issuing a Normal command.
    pub(super) running_help: bool,
    /// Function name called by the next g@ operator.
    operatorfunc: String,
    /// Source assignment of the next g@ operator.
    operatorfunc_origin: Option<ExOrigin>,
    /// Definition taking input across consecutive command lines.
    defining: Option<Definition>,
    /// Active script invocations, including nested calls and :normal! commands.
    depth: usize,
    /// Recent visible output, to which :echon appends its next fragment.
    echo_tail: super::ExMessageOutput,
    /// Variables local to the editor pane and scope handle for its current Tabpage.
    variables: control::WindowVariables,
    /// Origin of the script command currently being dispatched, when it came from :source.
    source: Option<String>,
}

/// Definitions and variables shared by every editor using the workspace's mappings.
#[derive(Default)]
pub(super) struct Shared {
    variables: control::Variables,
    user_commands: user_commands::UserCommands,
    /// Runtime event handlers registered by Ex scripts.
    autocmds: autocmd::Registry,
    /// Function definitions available to scripts throughout the workspace.
    functions: BTreeMap<String, Arc<Function>>,
}

impl Register {
    /// A later :echon starts a fresh line after a displayed Ex error.
    pub(super) fn reset_ex_echo_line(&mut self) {
        self.scripts.echo_tail = super::ExMessageOutput::default();
    }

    /// Borrow an owned handle to a declared function without locking during execution.
    fn script_function(&self, name: &str) -> Option<Arc<Function>> {
        self.input_maps
            .shared_scripts()
            .read()
            .unwrap()
            .functions
            .get(name)
            .cloned()
    }
    /// Attach the current Tabpage variable scope without changing pane-local bindings.
    pub fn set_script_tab_scope(&mut self, scope: ScriptTabScope) {
        self.scripts.variables.tab = scope;
    }

    /// Resolve a nonlocal expression variable for the given buffer.
    pub(super) fn script_variable(&self, buffer: u64, name: &str) -> Option<Value> {
        let (scope, key) = name.split_once(':').unwrap_or(("g", name));
        if matches!(scope, "w" | "t") {
            return self.scripts.variables.lookup(scope, key);
        }
        self.input_maps
            .shared_scripts()
            .read()
            .unwrap()
            .variables
            .lookup(buffer, scope, key)
    }
}

/// A callable Vim function with positional parameters and Ex statements.
struct Function {
    arguments: Vec<String>,
    statements: Vec<ScriptLine>,
    source: Option<String>,
    /// Location of the declaration, independent of its invocation site.
    defined_at: Option<ExOrigin>,
}

/// Pending function input and whether its existing name can be replaced.
struct Definition {
    name: String,
    overwrite: bool,
    function: Function,
}

impl Vim {
    /// Produce numbered statements while preserving source continuations and literal text bodies.
    pub fn script_lines(&self, text: &str, first: usize) -> Vec<ScriptLine> {
        let mut input = text
            .trim_start_matches('\u{feff}')
            .lines()
            .enumerate()
            .peekable();
        let mut lines = Vec::new();
        let mut function = false;
        let mut literal_text = false;
        while let Some((index, physical)) = input.next() {
            let number = first + index;
            if literal_text {
                literal_text = physical != ".";
                lines.push(ScriptLine {
                    number,
                    text: physical.into(),
                    literal: true,
                });
                continue;
            }
            let mut logical = physical
                .trim()
                .trim_start_matches(':')
                .trim_start()
                .to_owned();
            while let Some(continuation) = input
                .peek()
                .and_then(|(_, next)| next.trim_start().strip_prefix('\\'))
            {
                logical.push_str(continuation);
                input.next();
            }
            let logical = logical.trim();
            if logical.is_empty() || logical.starts_with('"') || logical.starts_with("#!") {
                continue;
            }
            let (verb, arguments) = split_word(logical);
            let name = verb.trim_end_matches('!');
            let autocmd_body = name.len() >= 2
                && "autocmd".starts_with(name)
                && arguments
                    .split_whitespace()
                    .filter(|word| *word != "|")
                    .count()
                    >= 3
                && arguments.split_whitespace().nth(1) != Some("|")
                && arguments.split_whitespace().nth(2) != Some("|");
            let commands = if autocmd_body {
                vec![logical]
            } else {
                self.source_statements(logical)
            };
            for text in commands {
                let protect_function = function;
                if function && is_end_function(text) {
                    function = false;
                } else if matches!(
                    split_word(text).0.trim_end_matches('!'),
                    "function" | "func" | "fu"
                ) {
                    function = !statements(text).any(is_end_function);
                } else if !function {
                    let (_, _, body) = super::ex_window_modifiers::command_modifiers(text);
                    let body = body.trim_start_matches(|ch: char| !ch.is_ascii_alphabetic());
                    let verb = body
                        .split(|ch: char| !ch.is_ascii_alphabetic())
                        .next()
                        .unwrap_or("");
                    literal_text = !verb.is_empty()
                        && ["append", "change", "insert"]
                            .iter()
                            .any(|name| name.starts_with(verb));
                }
                lines.push(ScriptLine {
                    number,
                    text: text.into(),
                    literal: protect_function,
                });
            }
        }
        lines
    }

    /// Dispatch one yielded statement with its source identity attached to local definitions.
    pub fn execute_script_statement(
        &mut self,
        program: &ScriptProgram,
        line: &ScriptLine,
        register: &mut Register,
    ) -> Action {
        if self.collecting_ex_lines() {
            self.accept_ex_line(&line.text);
            return Action::None;
        }
        let old = std::mem::replace(&mut register.scripts.source, program.source());
        let _origin = register
            .input_maps
            .enter_ex_origin(program.ex_origin(line.number));
        let action = if defining_function(register) {
            register.input_maps.trace_ex_line(&line.text);
            self.clear_ex_message();
            self.ex_failed = false;
            let mode = self.mode;
            let action = self
                .script_command(&line.text, register)
                .unwrap_or(Action::None);
            self.mode = mode;
            self.command.clear();
            action
        } else {
            self.execute_ex_line(&line.text, register)
        };
        register.scripts.source = old;
        action
    }

    /// Discard a function definition when its command-line input is canceled.
    pub(super) fn cancel_function_definition(register: &mut Register) {
        register.scripts.defining = None;
    }

    /// Handle script configuration or a command in `source` using this pane's `register`.
    pub(super) fn script_command(
        &mut self,
        source: &str,
        register: &mut Register,
    ) -> Option<Action> {
        let (command, argument) = split_word(source);
        if register.scripts.defining.is_none() {
            if let Some(action) = self.script_autocmd_command(source, register) {
                return Some(action);
            }
            if let Some(encoding) = encoding::source_encoding(source) {
                if register.scripts.source.is_none() {
                    self.script_error(":scriptencoding requires :source");
                } else if encoding.split_whitespace().count() > 1 {
                    self.script_error("Invalid encoding argument");
                }
                return Some(Action::None);
            }
            if let Some(action) = self.user_ex_command(source, register) {
                return Some(action);
            }
            if matches!(command, "function" | "func" | "fu") && !argument.contains('(') {
                return Some(self.list_script_functions(argument, register));
            }
            if command.len() >= 4 && "finish".starts_with(command) && argument.is_empty() {
                return Some(Action::FinishSource);
            }
            let name = command.trim_start_matches(|ch: char| ch.is_ascii_digit());
            let address = command[..command.len() - name.len()].parse().ok();
            if name.len() >= 3 && "scriptnames".starts_with(name) {
                return Some(if address.is_none() && argument.is_empty() {
                    Action::ListScripts
                } else {
                    Action::EditSourcedScript {
                        address,
                        path: (!argument.is_empty()).then(|| argument.into()),
                    }
                });
            }
        }
        self.try_script_command(source, register).map(|result| {
            if let Err(error) = result {
                self.script_error(&error);
            }
            Action::None
        })
    }

    /// Display local function declarations and the body of a selected function.
    fn list_script_functions(&mut self, name: &str, register: &Register) -> Action {
        let verbose = register.input_maps.ex_verbose() > 0;
        let shared = register.input_maps.shared_scripts();
        let shared = shared.read().unwrap();
        let mut output = String::new();
        for (function_name, function) in &shared.functions {
            if !name.is_empty() && name != function_name {
                continue;
            }
            let _ = write!(
                output,
                "function {function_name}({})",
                function.arguments.join(", ")
            );
            if verbose && let Some(origin) = &function.defined_at {
                origin.append_to(&mut output);
            }
            output.push('\n');
            if !name.is_empty() {
                for (index, statement) in function.statements.iter().enumerate() {
                    let _ = writeln!(output, "{:4}  {}", index + 1, statement.text);
                }
                output.push_str("   endfunction\n");
            }
        }
        if !name.is_empty() && output.is_empty() {
            self.script_error(&format!("Unknown function: {name}"));
            Action::None
        } else {
            Action::ShowListing(output)
        }
    }

    /// Dispatch only script commands, returning None for the regular Ex dispatcher.
    fn try_script_command(
        &mut self,
        source: &str,
        register: &mut Register,
    ) -> Option<Result<(), String>> {
        let source = source.trim();
        let origin = register.input_maps.current_ex_origin();
        let number = origin.as_ref().map_or(1, ExOrigin::line);
        if let Some(mut pending) = register.scripts.defining.take() {
            if is_end_function(source) {
                return Some(install(pending, register));
            }
            if !source.is_empty() {
                pending
                    .function
                    .statements
                    .extend(statements(source).map(|text| ScriptLine {
                        number,
                        text: text.into(),
                        literal: false,
                    }));
            }
            register.scripts.defining = Some(pending);
            self.open_prompt(':', 1);
            return Some(Ok(()));
        }
        let (command, argument) = split_word(source);
        if matches!(command.trim_end_matches('!'), "function" | "func" | "fu") {
            return Some((|| {
                let mut lines = statements(argument);
                let declaration = lines.next().ok_or("Missing function declaration")?;
                let (name, args) = call_parts(declaration.trim_end_matches(" abort").trim())
                    .ok_or("Expected a function name followed by its arguments")?;
                if !is_function_name(name) {
                    return Err(format!("Invalid function name: {name}"));
                }
                let arguments = args
                    .split(',')
                    .map(str::trim)
                    .filter(|arg| !arg.is_empty())
                    .map(str::to_owned)
                    .collect();
                let mut pending = Definition {
                    name: name.into(),
                    overwrite: command.ends_with('!'),
                    function: Function {
                        arguments,
                        statements: Vec::new(),
                        source: register.scripts.source.clone(),
                        defined_at: origin,
                    },
                };
                for line in lines {
                    if is_end_function(line) {
                        return install(pending, register);
                    }
                    pending.function.statements.push(ScriptLine {
                        number,
                        text: line.into(),
                        literal: false,
                    });
                }
                register.scripts.defining = Some(pending);
                self.open_prompt(':', 1);
                Ok(())
            })());
        }
        if matches!(command, "set" | "se")
            && let Some(argument) = argument
                .strip_prefix("operatorfunc")
                .or_else(|| argument.strip_prefix("opfunc"))
        {
            if argument.is_empty() || argument == "?" {
                self.message = format!("operatorfunc={}", register.scripts.operatorfunc);
                if register.input_maps.ex_verbose() > 0
                    && let Some(origin) = &register.scripts.operatorfunc_origin
                {
                    origin.append_inline(&mut self.message);
                }
            } else if let Some(name) = argument.strip_prefix('=') {
                register.scripts.operatorfunc = name.trim().into();
                register.scripts.operatorfunc_origin = origin;
            } else {
                return Some(Err("Invalid operatorfunc option".into()));
            }
            return Some(Ok(()));
        }
        if matches!(command, "let")
            && let Some((setting, value)) = argument.split_once('=')
            && matches!(setting.trim(), "&operatorfunc" | "&opfunc")
        {
            return Some(
                self.evaluate_expression(value.trim(), register)
                    .map(|value| {
                        register.scripts.operatorfunc = value.display();
                        register.scripts.operatorfunc_origin = origin;
                    }),
            );
        }
        (matches!(
            command,
            "echo"
                | "echomsg"
                | "echom"
                | "echoerr"
                | "echoe"
                | "echon"
                | "eval"
                | "eva"
                | "ev"
                | "call"
                | "execute"
                | "exe"
                | "normal"
                | "normal!"
                | "break"
                | "brea"
                | "continue"
                | "con"
                | "throw"
                | "th"
                | "defer"
                | "const"
                | "cons"
                | "unlet"
                | "unl"
                | "unle"
                | "unlet!"
                | "unl!"
                | "unle!"
                | "lockvar"
                | "lockv"
                | "lockva"
                | "lockvar!"
                | "unlockvar"
                | "unlo"
                | "unloc"
                | "unlockv"
                | "unlockva"
                | "unlockvar!"
        ) || is_delete_function(command))
        .then(|| self.run_script(source, register))
        .or_else(|| {
            let words = [
                "if", "elseif", "else", "endif", "while", "endwhile", "for", "endfor", "try",
                "catch", "finally", "endtry",
            ];
            let (word, _) = split_word(statements(source).next().unwrap_or(source));
            if words.contains(&control::keyword(word)) {
                return Some(self.run_script(source, register));
            }
            if command == "let"
                && argument.split_once('=').is_some_and(|(name, _)| {
                    !name.trim().starts_with('&')
                        && !matches!(name.trim(), "mapleader" | "g:mapleader")
                })
            {
                return Some(self.run_script(source, register));
            }
            None
        })
    }

    /// Publish the operated `ranges` and pass `kind` to the configured operator in `register`.
    pub(super) fn invoke_operator(
        &mut self,
        kind: &str,
        ranges: Vec<Range<usize>>,
        register: &mut Register,
    ) {
        let Some(start) = ranges.first().map(|range| range.start) else {
            return;
        };
        let end = ranges.last().expect("operator ranges exist").end;
        self.set_operated_range(start..end);
        self.mode = Mode::Normal;
        self.set_cursor(start);
        let name = register.scripts.operatorfunc.clone();
        if name.is_empty() {
            self.message = i18n::text("vim_g.operator_missing").into();
            return;
        }
        let Some(function) = register.script_function(&name) else {
            self.script_error(&format!("Unknown function: {name}"));
            return;
        };
        if function.arguments.len() != 1 {
            self.script_error(&format!(
                "The operator function {name} must accept one argument"
            ));
            return;
        }
        let selected = if kind == "block" {
            ranges
                .iter()
                .map(|range| &self.doc.text[range.clone()])
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            self.doc.text[start..end].into()
        };
        let locals = BTreeMap::from([
            ("a:text".into(), Value::String(selected)),
            (
                format!("a:{}", function.arguments[0]),
                Value::String(kind.into()),
            ),
        ]);
        if let Err(error) = self.run_lines(
            function.statements.iter().cloned(),
            locals,
            function.source.clone(),
            register,
        ) {
            self.script_error(&error);
        }
    }

    /// Execute supported Vim statements in `source` against this editor and its `register`.
    pub(super) fn run_script(
        &mut self,
        source: &str,
        register: &mut Register,
    ) -> Result<(), String> {
        let mut statements = control::program_lines(source);
        if let Some(origin) = register.input_maps.current_ex_origin() {
            for line in &mut statements {
                line.number += origin.line().saturating_sub(1);
            }
        }
        let program = ScriptProgram::compile(statements, register.scripts.source.clone())
            .map_err(|(_, error)| error)?;
        let run = || self.run_program(program, register).map(|_| ());
        if matches!(split_word(source).0, "execute" | "exe") {
            super::ex_window_modifiers::without_ex_keep_jumps(run)
        } else {
            run()
        }
    }

    /// Run `lines` with scoped `locals`, bounding recursive invocations through `register`.
    fn run_lines(
        &mut self,
        lines: impl IntoIterator<Item = ScriptLine>,
        arguments: BTreeMap<String, Value>,
        source: Option<String>,
        register: &mut Register,
    ) -> Result<Option<Value>, String> {
        let program =
            ScriptProgram::function(lines, arguments, source).map_err(|(_, error)| error)?;
        super::ex_window_modifiers::without_ex_keep_jumps(|| self.run_program(program, register))
    }

    /// Execute a program locally, feeding any command failures back into its active try frame.
    fn run_program(
        &mut self,
        mut program: ScriptProgram,
        register: &mut Register,
    ) -> Result<Option<Value>, String> {
        if register.scripts.depth >= 16 {
            return Err("Vim script call depth exceeded".into());
        }
        register.scripts.depth += 1;
        let old_source = std::mem::replace(&mut register.scripts.source, program.source());
        let result = (|| {
            let mut error = None;
            while let Some(line) = program
                .next(self, register, error.take())
                .map_err(|(_, error)| error)?
            {
                let bindings = program.bindings(register);
                let _origin = register
                    .input_maps
                    .enter_ex_origin(program.ex_origin(line.number));
                error = self.script_line(&line.text, &bindings, register).err();
            }
            Ok(program.returned())
        })();
        let mut deferred_error = None;
        for (name, arguments) in program.deferred() {
            if let Err(error) = self.script_call(&name, arguments, register) {
                deferred_error.get_or_insert(error);
            }
        }
        register.scripts.source = old_source;
        register.scripts.depth -= 1;
        result.and_then(|value| deferred_error.map_or(Ok(value), Err))
    }

    /// Execute one `line` and return the value that ends its current `locals` scope.
    fn script_line(
        &mut self,
        line: &str,
        locals: &BTreeMap<String, Value>,
        register: &mut Register,
    ) -> Result<(), String> {
        if let Some((modifiers, command)) = super::ex_window_modifiers::parse_modifiers(line)
            && !modifiers.has_native_options()
            && !command.is_empty()
        {
            let mappings = register.input_maps.clone();
            return modifiers
                .with_ex_state(&mappings, || self.script_line(command, locals, register));
        }
        let (command, rest) = split_word(line);
        let evaluate =
            |vim: &Vim, source: &str, register: &Register, locals: &BTreeMap<String, Value>| {
                let bindings = locals
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.clone()))
                    .collect::<Vec<_>>();
                vim.evaluate_expression_with(source, register, &bindings)
            };
        match command {
            "" => {}
            key if key.starts_with('"') => {}
            "echo" | "echomsg" | "echom" => {
                let value = self.ex_message(rest, register, locals, " ")?;
                let output = super::ExMessageOutput::with_style(
                    &value,
                    register.input_maps.echo_highlight(),
                );
                self.record_script_output(register, &value, true);
                if command != "echo" {
                    register
                        .input_maps
                        .remember_styled_ex_message(output.clone());
                }
                self.publish_echo_message(output, register);
            }
            "echon" => {
                let value = self.ex_message(rest, register, locals, "")?;
                self.record_script_output(register, &value, false);
                let mut output = register.scripts.echo_tail.clone();
                output.push(&value, register.input_maps.echo_highlight());
                self.publish_echo_message(output, register);
            }
            "echoerr" | "echoe" => return Err(self.ex_message(rest, register, locals, " ")?),
            "eval" | "eva" | "ev" => {
                if let Some((name, args)) = call_parts(rest)
                    && register.script_function(name).is_some()
                {
                    let args = split_fields(args, ',')
                        .into_iter()
                        .filter(|value| !value.is_empty())
                        .map(|value| evaluate(self, value, register, locals))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.script_call(name, args, register)?;
                } else {
                    evaluate(self, rest, register, locals)?;
                }
            }
            name if is_delete_function(name) => {
                if !is_function_name(rest) {
                    return Err("Expected a function name".into());
                }
                if register
                    .input_maps
                    .shared_scripts()
                    .write()
                    .unwrap()
                    .functions
                    .remove(rest)
                    .is_none()
                    && !name.ends_with('!')
                {
                    return Err(format!("Unknown function: {rest}"));
                }
            }
            "normal" | "normal!" => self.script_normal(rest, command == "normal", register)?,
            "call" => {
                let (name, args) = call_parts(rest).ok_or("Invalid function call")?;
                let args = split_fields(args, ',')
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .map(|value| evaluate(self, value, register, locals))
                    .collect::<Result<Vec<_>, _>>()?;
                self.script_call(name, args, register)?;
            }
            _ => {
                let previous = self.displayed_ex_message();
                let action = self.execute_ex_line(line, register);
                if self.ex_failed() {
                    return Err(self.message.clone());
                }
                match action {
                    Action::None => {}
                    Action::ShowListing(listing)
                        if register.input_maps.capturing_ex_output()
                            || register.input_maps.ex_silence().is_some() =>
                    {
                        self.record_script_output(register, &listing, true);
                        if register.input_maps.ex_silence().is_none() {
                            self.message = listing;
                        }
                    }
                    Action::ShowStyledListing(listing)
                        if register.input_maps.capturing_ex_output()
                            || register.input_maps.ex_silence().is_some() =>
                    {
                        self.record_script_output(register, &listing.text, true);
                        if register.input_maps.ex_silence().is_none() {
                            self.publish_styled_ex_message(listing);
                        }
                    }
                    Action::MessageModified { modifier, command } => {
                        let mappings = register.input_maps.clone();
                        mappings.begin_ex_message_modifier(modifier);
                        let silence = mappings.ex_silence();
                        let sequence = mappings.ex_output_sequence();
                        let result = self.script_line(&command, locals, register);
                        if let Err(error) = &result
                            && silence == Some(true)
                        {
                            if mappings.ex_output_sequence() == sequence {
                                mappings.record_ex_output(error, true);
                            }
                            mappings.remember_ex_error(error);
                            self.ex_failed = false;
                        }
                        if silence.is_some() {
                            self.publish_styled_ex_message(previous);
                        } else {
                            let output =
                                mappings.filter_styled_ex_output(self.displayed_ex_message());
                            self.publish_styled_ex_message(output);
                        }
                        mappings.end_ex_message_modifier();
                        if silence != Some(true) {
                            result?;
                        }
                    }
                    Action::UserEx {
                        text,
                        source,
                        source_line,
                    } => {
                        let lines = self.script_lines(&text, source_line);
                        let program = ScriptProgram::user_command(lines, source)
                            .map_err(|(_, error)| error)?;
                        self.run_program(program, register)?;
                    }
                    _ => {
                        return Err(format!(
                            "The Vim script command :{command} requires a native window action"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Record each script message, even when a function emits several in one Ex command.
    fn record_script_output(&self, register: &Register, output: &str, newline: bool) {
        let filtered = register.input_maps.filter_ex_output(output);
        if output.is_empty() || !filtered.is_empty() {
            register.input_maps.record_ex_output(&filtered, newline);
        }
    }

    /// Keep the visible line for :echon while silent output continues through :redir.
    fn publish_echo_message(&mut self, output: super::ExMessageOutput, register: &mut Register) {
        if register.input_maps.ex_silence().is_none() {
            register.scripts.echo_tail =
                register.input_maps.filter_styled_ex_output(output.clone());
        }
        self.publish_styled_ex_message(output);
    }

    /// Render `source` expressions separated by `separator` for an Ex message.
    fn ex_message(
        &self,
        source: &str,
        register: &Register,
        locals: &BTreeMap<String, Value>,
        separator: &str,
    ) -> Result<String, String> {
        let bindings = locals
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect::<Vec<_>>();
        Ok(self
            .evaluate_expression_list_with(source, register, &bindings)?
            .iter()
            .map(Value::display)
            .collect::<Vec<_>>()
            .join(separator))
    }

    /// Call a supported builtin or locally declared `name` with evaluated `arguments`.
    fn script_call(
        &mut self,
        name: &str,
        arguments: Vec<Value>,
        register: &mut Register,
    ) -> Result<(), String> {
        if let Some(function) = register.script_function(name) {
            if function.arguments.len() != arguments.len() {
                return Err(format!("Incorrect argument count for {name}"));
            }
            if register.input_maps.ex_verbose() >= 12 {
                let args = arguments
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join(", ");
                register
                    .input_maps
                    .trace_ex(12, &format!("calling {name}({args})"));
            }
            let locals = function
                .arguments
                .iter()
                .zip(arguments)
                .map(|(name, value)| (format!("a:{name}"), value))
                .collect();
            let result = self.run_lines(
                function.statements.iter().cloned(),
                locals,
                function.source.clone(),
                register,
            );
            if register.input_maps.ex_verbose() >= 12 {
                let outcome = match &result {
                    Ok(value) => value.as_ref().map_or_else(|| "0".into(), Value::display),
                    Err(message) => message.clone(),
                };
                register
                    .input_maps
                    .trace_ex(12, &format!("{name} returning {outcome}"));
            }
            return result.map(|_| ());
        }
        match (name, arguments.as_slice()) {
            ("cursor", [Value::Integer(line), Value::Integer(column)]) => {
                let row = line.saturating_sub(1).max(0) as usize;
                let start = self.doc.line_range(row.min(self.line_count() - 1));
                self.set_cursor(
                    start
                        .start
                        .saturating_add(column.saturating_sub(1).max(0) as usize)
                        .min(start.end),
                );
                Ok(())
            }
            ("setline", [Value::Integer(line), Value::String(text)]) if *line >= 1 => {
                let row = (*line as usize) - 1;
                if self.read_only || row >= self.line_count() || text.contains(['\r', '\n']) {
                    return Err("Cannot replace the requested line".into());
                }
                self.replace(self.doc.line_range(row), text, true);
                Ok(())
            }
            _ => Err(format!("Unsupported function call: {name}")),
        }
    }

    /// Feed Normal `keys`, running user mappings for :normal without a bang.
    fn script_normal(
        &mut self,
        keys: &str,
        remap: bool,
        register: &mut Register,
    ) -> Result<(), String> {
        let mut keys = keys.graphemes(true).map(|raw| match raw {
            "\u{1b}" => "Escape",
            "\u{16}" => "Ctrl+v",
            "\r" | "\n" => "Enter",
            "\t" => "Tab",
            _ => raw,
        });
        let action = if remap {
            self.dispatch_mapped_script_keys(keys.map(str::to_owned).collect(), register)
        } else {
            keys.find_map(|key| {
                let action = self.handle_key(key, register, &mut |_, _, _, _| None);
                (action != Action::None).then_some(action)
            })
            .unwrap_or(Action::None)
        };
        if action != Action::None {
            self.cancel_pending();
            return Err("This Vim script command requires a native window action".into());
        }
        if !self.substitute_confirmation_active()
            && (self.mode != Mode::Normal || self.awaiting_key())
        {
            self.handle_key("Escape", register, &mut |_, _, _, _| None);
        }
        Ok(())
    }

    /// Present script `error` on the editor status line.
    pub(super) fn script_error(&mut self, error: &str) {
        self.ex_message_spans.clear();
        self.ex_failed = true;
        self.message = i18n::format("vim_g.script_error", &[("error", &error)]);
    }
}

impl Register {
    /// Ex statements inherit the source identity of their containing script.
    pub(super) fn script_source(&self) -> Option<String> {
        self.scripts.source.clone()
    }
}

/// Recognize the full or native shortened :delfunction spelling with an optional bang.
fn is_delete_function(word: &str) -> bool {
    let name = word.trim_end_matches('!');
    name.len() >= 4 && "delfunction".starts_with(name)
}

/// Register the completed `definition`, honoring function! replacement.
fn install(definition: Definition, register: &Register) -> Result<(), String> {
    let shared = register.input_maps.shared_scripts();
    let mut shared = shared.write().unwrap();
    if !definition.overwrite && shared.functions.contains_key(&definition.name) {
        return Err(format!("Function already exists: {}", definition.name));
    }
    shared
        .functions
        .insert(definition.name, Arc::new(definition.function));
    Ok(())
}

/// Recognize the full and common abbreviated function terminators in `line`.
fn is_end_function(line: &str) -> bool {
    matches!(line.trim(), "endfunction" | "endfunc" | "endf")
}

/// Validate a Vim function `name` before adding it to the pane.
fn is_function_name(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Split function call `source` into its name and inner argument text.
fn call_parts(source: &str) -> Option<(&str, &str)> {
    let (name, rest) = source.split_once('(')?;
    Some((name.trim(), rest.strip_suffix(')')?))
}

/// Split an Ex `source` into its first word and trimmed argument.
fn split_word(source: &str) -> (&str, &str) {
    let source = source.trim().trim_start_matches(':').trim_start();
    source
        .split_once(char::is_whitespace)
        .map_or((source, ""), |(word, rest)| (word, rest.trim_start()))
}

/// Return Ex statements in `source`, preserving quoted bars and logical OR.
pub(super) fn statements(source: &str) -> impl Iterator<Item = &str> {
    split_fields(source, '|')
        .into_iter()
        .filter(|line| !line.is_empty())
}

/// Whether the current editor is collecting a multiline function body.
pub(super) fn defining_function(register: &Register) -> bool {
    register.scripts.defining.is_some()
}

/// Split `source` at top-level `delimiter` characters outside quoted values.
fn split_fields(source: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = None;
    let mut brackets = 0usize;
    let protected_prefix = |start| {
        if delimiter == '|' {
            let command = &source[start..];
            let sign = super::ex_sign::protected_prefix(command);
            start
                + if sign > 0 {
                    sign
                } else {
                    super::ex_match::protected_prefix(command)
                }
        } else {
            0
        }
    };
    let mut protected_end = protected_prefix(start);
    let mut input = source.char_indices().peekable();
    while let Some((at, ch)) = input.next() {
        if at < protected_end {
            continue;
        }
        if let Some(quote) = quoted {
            if ch == '\\' && quote == '"' {
                input.next();
            } else if ch == quote {
                if quote == '\'' && input.peek().is_some_and(|(_, ch)| *ch == '\'') {
                    input.next();
                } else {
                    quoted = None;
                }
            }
            continue;
        }
        match ch {
            '\\' => {
                input.next();
            }
            '"' if delimiter == '|'
                && source[start..at].strip_suffix('@').is_some_and(|prefix| {
                    prefix
                        .split_whitespace()
                        .last()
                        .is_some_and(|word| matches!(word.trim_end_matches('!'), "redi" | "redir"))
                }) => {}
            '\'' | '"' => quoted = Some(ch),
            '(' | '[' | '{' => brackets += 1,
            ')' | ']' | '}' => brackets = brackets.saturating_sub(1),
            ch if ch == delimiter && brackets == 0 => {
                if delimiter == '|' && input.peek().is_some_and(|(_, ch)| *ch == '|') {
                    input.next();
                    continue;
                }
                parts.push(source[start..at].trim());
                start = at + ch.len_utf8();
                protected_end = protected_prefix(start);
            }
            _ => {}
        }
    }
    parts.push(source[start..].trim());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn rust_script_uses_nested_loops_catches_and_finally_on_continue() {
        let mut vim = Vim::new(Document::new("first\nsecond\n"));
        let mut register = Register::default();
        vim.run_script(
            "let g:sum = 0\nlet g:index = 0\nwhile g:index < 5\nlet g:index += 1\ntry\nif g:index == 2\ncontinue\nelseif g:index == 4\nbreak\nelse\nlet g:sum += g:index\nendif\nfinally\nlet g:sum += 10\nendtry\nendwhile\nfor [label, number] in [['X', 2], ['Y', 3]]\nlet g:sum += number\nendfor\ntry\nthrow 'replace me'\ncatch /replace/\nlet g:result = v:exception\nfinally\nlet g:result .= '!'\nendtry\necho g:sum",
            &mut register,
        ).unwrap();
        assert_eq!(vim.message, "49");
        vim.execute_ex_line("echo g:result", &mut register);
        assert_eq!(vim.message, "replace me!");
    }

    #[test]
    fn sourced_vm_yields_commands_and_can_catch_native_failures() {
        let mut vim = Vim::new(Document::new("one\ntwo\n"));
        let mut register = Register::default();
        let source = "let s:ready = 0\ntry\n1read missing.txt\ncatch /missing/\nlet s:ready = 1\nfinally\nlet g:done = s:ready\nendtry\nif g:done\nappend\nif string literal\n.\nendif\nfunction! SourceValue()\nif s:ready\necho 'ready'\nendif\nendfunction\ncall SourceValue()";
        let script = "test-script.vim".to_owned();
        let mut program =
            ScriptProgram::compile(vim.script_lines(source, 1), Some(script)).unwrap();
        let host_action = program.next(&vim, &mut register, None).unwrap().unwrap();
        assert_eq!(host_action.text, "1read missing.txt");
        assert_ne!(
            vim.execute_script_statement(&program, &host_action, &mut register),
            Action::None
        );
        let mut failure = Some("missing file".into());
        while let Some(line) = program.next(&vim, &mut register, failure.take()).unwrap() {
            assert_eq!(
                vim.execute_script_statement(&program, &line, &mut register),
                Action::None
            );
            assert!(!vim.ex_failed(), "{}: {}", line.number, vim.message);
        }
        assert_eq!(vim.doc.text, "one\nif string literal\ntwo\n");
        assert_eq!(vim.message, "ready");
    }

    #[test]
    fn scoped_variables_locks_and_deferred_calls_are_evaluated_locally() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("let g:log = ''", &mut register);
        vim.execute_ex_line(
            "function! Push(v) | let g:log .= a:v | endfunction",
            &mut register,
        );
        vim.execute_ex_line("function! Run(v) | let local = a:v | defer Push(local) | let local = 'changed' | defer Push('B') | try | return 7 | finally | let g:log .= 'F' | endtry | endfunction", &mut register);
        vim.execute_ex_line("call Run('A')", &mut register);
        assert!(!vim.ex_failed(), "{}", vim.message);
        vim.execute_ex_line("echo g:log", &mut register);
        assert_eq!(vim.message, "FBA");

        vim.execute_ex_line("const g:limit = 9", &mut register);
        vim.execute_ex_line("let g:limit = 1", &mut register);
        assert!(vim.ex_failed());
        vim.execute_ex_line("let b:item = 'buffer'", &mut register);
        vim.execute_ex_line("lockvar b:item", &mut register);
        vim.execute_ex_line("let b:item = 'bad'", &mut register);
        assert!(vim.ex_failed());
        vim.execute_ex_line("unlockvar b:item", &mut register);
        vim.execute_ex_line("unlet b:item", &mut register);
        vim.execute_ex_line("echo b:item", &mut register);
        assert!(vim.ex_failed());
        vim.execute_ex_line("unlet! b:item", &mut register);
        assert!(!vim.ex_failed());
    }

    #[test]
    fn user_commands_expand_arguments_and_keep_buffers_local() {
        let mut vim = Vim::new(Document::new("one\ntwo\n"));
        let mut register = Register::default();
        vim.execute_ex_line("command! -range -bang -nargs=* -register -bar Greet echo <q-args> .. '<bang>' | echo <line1> .. ':' .. <line2> .. ':<reg>'", &mut register);
        let Action::UserEx { text, source, .. } =
            vim.execute_ex_line("1,2Greet! a hello world | echo 3", &mut register)
        else {
            panic!("{}", vim.message)
        };
        assert_eq!(
            text,
            "echo \"hello world\" .. '!' | echo 1 .. ':' .. 2 .. ':a' | echo 3"
        );
        assert!(source.is_none());
        vim.execute_ex_line("command -buffer Local echo 1", &mut register);
        assert_eq!(vim.user_ex_candidates("Lo", &register), ["Local"]);
        let other = Vim::new(Document::new(""));
        assert!(other.user_ex_candidates("Lo", &register).is_empty());
        assert_eq!(other.user_ex_candidates("Gr", &register), ["Greet"]);
        vim.execute_ex_line("delcommand -buffer Local", &mut register);
        assert!(vim.user_ex_candidates("Lo", &register).is_empty());
        vim.execute_ex_line("comclear", &mut register);
        assert!(vim.user_ex_candidates("Gr", &register).is_empty());
    }

    /// Define a function through Ex and check all three operator shapes and the operated marks.
    #[test]
    fn custom_operator_receives_shapes_and_motion_marks() {
        let mut vim = Vim::new(Document::new("abc\ndef\nghi\n"));
        let mut register = Register::default();
        vim.text(
            ":function! ShowShape(kind) | echo a:kind .. ':' .. a:text | endfunction",
            &mut register,
        );
        vim.key("Enter", &mut register);
        vim.text(":set operatorfunc=ShowShape", &mut register);
        vim.key("Enter", &mut register);
        vim.text("g@iw", &mut register);
        assert_eq!(vim.message, "char:abc");
        vim.text("g@@", &mut register);
        assert_eq!(vim.message, "line:abc\n");
        vim.key("Ctrl+v", &mut register);
        vim.text("ljg@", &mut register);
        assert_eq!(vim.message, "block:ab\nde");
        vim.text("`]", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (1, 1));
    }

    /// Apply a function's real editing commands over the operated range and undo in one step.
    #[test]
    fn custom_operator_can_edit_only_the_selected_word() {
        let mut vim = Vim::new(Document::new("one two\n"));
        let mut register = Register::default();
        vim.text(
            ":function! Upper(kind) | normal! `[v`]U | endfunction",
            &mut register,
        );
        vim.key("Enter", &mut register);
        vim.text(":let &operatorfunc = 'Upper'", &mut register);
        vim.key("Enter", &mut register);
        vim.text("wg@iw", &mut register);
        assert_eq!(vim.doc.text, "one TWO\n");
        vim.text("u", &mut register);
        assert_eq!(vim.doc.text, "one two\n");
    }
}
