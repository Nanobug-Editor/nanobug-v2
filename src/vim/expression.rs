//! Typed Vim expressions used by command prompts and programmable editor operations.

mod parser;

use super::{Action, Mode, Register, ScreenMotionResolver, Vim};
use crate::i18n;
use std::{cell::Cell, collections::BTreeMap};

/// Maximum bytes of expanded text in a single expression and its intermediate values.
const VALUE_LIMIT: usize = 2 * 1024 * 1024;

/// A Vim expression value with its original type.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value {
    /// A signed 64-bit Vim Number.
    Integer(i64),
    /// A Vim Float.
    Float(f64),
    /// A Vim Boolean.
    Bool(bool),
    /// A Vim null value.
    Null,
    /// UTF-8 text.
    String(String),
    /// Ordered values.
    List(Vec<Value>),
    /// Values addressed by string keys.
    Dictionary(BTreeMap<String, Value>),
}

impl Value {
    /// Render a value as text, leaving strings unquoted for insertion.
    pub(super) fn display(&self) -> String {
        match self {
            Self::Integer(n) => n.to_string(),
            Self::Float(n) => {
                let mut text = n.to_string();
                if n.is_finite() && !text.contains(['.', 'e', 'E']) {
                    text.push_str(".0");
                }
                text
            }
            Self::Bool(value) => format!("v:{value}"),
            Self::Null => "v:null".into(),
            Self::String(text) => text.clone(),
            Self::List(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::quoted)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Dictionary(values) => format!(
                "{{{}}}",
                values
                    .iter()
                    .map(|(key, value)| format!(
                        "{}: {}",
                        Self::String(key.clone()).quoted(),
                        value.quoted()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Render this value as its Vim literal, including quotes for strings.
    fn quoted(&self) -> String {
        if let Self::String(text) = self {
            format!("'{}'", text.replace('\'', "''"))
        } else {
            self.display()
        }
    }

    /// Bound the rendered size before expanding lists, cloning locals, or concatenating text.
    fn size_hint(&self) -> usize {
        match self {
            Self::String(text) => text.len().saturating_mul(2).saturating_add(2),
            Self::List(items) => items.iter().fold(2usize, |total, item| {
                total.saturating_add(2).saturating_add(item.size_hint())
            }),
            Self::Dictionary(items) => items.iter().fold(2usize, |total, (key, item)| {
                total
                    .saturating_add(6)
                    .saturating_add(key.len().saturating_mul(2))
                    .saturating_add(item.size_hint())
            }),
            _ => 32,
        }
    }

    /// Coerce a scalar to a Vim Number where used as an integer argument.
    pub(super) fn number(&self) -> Result<i64, String> {
        match self {
            Self::Integer(n) => Ok(*n),
            Self::Bool(value) => Ok(i64::from(*value)),
            Self::Null => Ok(0),
            Self::String(text) => Ok(parser::string_number(text)),
            _ => Err("Expected a Number".into()),
        }
    }

    /// Produce insertion contents and whether they represent full lines.
    fn register_contents(&self, put: bool) -> super::register::Contents {
        use super::register::{Contents, Kind};
        let mut linewise = false;
        let mut text = match self {
            Self::List(items) => {
                linewise = !items.is_empty();
                items
                    .iter()
                    .map(Self::display)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => self.display(),
        };
        if linewise {
            text.push('\n');
        }
        if put && text.ends_with('\n') {
            linewise = true;
        }
        Contents {
            text,
            kind: if linewise {
                Kind::Line
            } else {
                Kind::Character
            },
        }
    }
}

/// The operation that consumes the next expression.
pub(super) enum Target {
    /// Insert the result as typed register text, optionally with a literal modifier.
    Insert(Option<char>),
    /// Replace the saved outer command line.
    ReplaceLine,
    /// Feed the result through Normal-mode register playback with the supplied count.
    Execute(usize),
    /// Set the expression register for the next put with the saved count.
    Select(Option<usize>),
}

/// A nested expression prompt, holding the complete outer text-input state.
pub(super) struct Prompt {
    /// Consumer selected by the prefix that opened this prompt.
    target: Target,
    /// Mode to resume after submitting or canceling.
    mode: Mode,
    /// Type of the saved outer prompt.
    kind: char,
    /// Draft and its UTF-8 caret.
    command: String,
    caret: usize,
    /// Candidate, history, and search preview state belonging to the outer prompt.
    state: super::command_prompt::State,
    /// Whether the saved outer draft was in persistent Ex mode.
    ex_mode: bool,
    /// Whether the saved Visual-shaped mode was Select.
    select: bool,
}

impl Vim {
    /// Evaluate a Vim expression against this buffer and the supplied register bank.
    pub(super) fn evaluate_expression(
        &self,
        source: &str,
        register: &Register,
    ) -> Result<Value, String> {
        self.evaluate_expression_with(source, register, &[])
    }

    /// Evaluate source using read-only editor data and the provided named local bindings.
    pub(super) fn evaluate_expression_with(
        &self,
        source: &str,
        register: &Register,
        locals: &[(&str, Value)],
    ) -> Result<Value, String> {
        parser::evaluate(self, source, register, locals, None)
    }

    /// Evaluate the adjacent source expressions used by Ex output commands.
    pub(super) fn evaluate_expression_list_with(
        &self,
        source: &str,
        register: &Register,
        locals: &[(&str, Value)],
    ) -> Result<Vec<Value>, String> {
        parser::evaluate_list(self, source, register, locals)
    }

    /// Replace the visible prompt with an expression while preserving its consumer and outer state.
    pub(super) fn begin_expression(&mut self, target: Target) {
        if self.expression_prompt.is_some() {
            return;
        }
        let saved = self.command_prompt.take_navigation();
        self.expression_prompt = Some(Prompt {
            target,
            mode: self.mode,
            kind: self.prompt,
            command: std::mem::take(&mut self.command),
            caret: self.command_pos,
            state: saved,
            ex_mode: self.g.ex_mode,
            select: self.g.select,
        });
        self.pending = None;
        self.mode = Mode::Command;
        self.prompt = '=';
        self.command_pos = 0;
        self.g.select = false;
        self.g.ex_mode = false;
    }

    /// Close the expression and restore the earlier prompt or editing mode.
    fn restore_expression(&mut self) -> Option<Target> {
        let saved = self.expression_prompt.take()?;
        self.pending = None;
        self.mode = saved.mode;
        self.prompt = saved.kind;
        self.command = saved.command;
        self.command_pos = saved.caret;
        self.command_prompt.restore_navigation(saved.state);
        self.g.select = saved.select;
        self.g.ex_mode = saved.ex_mode;
        Some(saved.target)
    }

    /// Cancel a nested expression prompt, allowing outer command editing to continue.
    pub(super) fn cancel_expression(&mut self) -> bool {
        self.restore_expression().is_some()
    }

    /// Evaluate the expression and deliver its value to the prefix that opened the prompt.
    pub(super) fn submit_expression(
        &mut self,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let mut source = std::mem::take(&mut self.command);
        let Some(target) = self.restore_expression() else {
            return Action::None;
        };
        let replace = matches!(target, Target::ReplaceLine);
        if source.is_empty() && !replace {
            source.clone_from(&register.expression_source);
        }
        if source.is_empty() {
            return Action::None;
        }
        register.command_history.remember('=', &source);
        if !replace {
            register.expression_source.clone_from(&source);
        }
        let command_pos = Cell::new(None);
        let value = match parser::evaluate(
            self,
            &source,
            register,
            &[],
            replace.then_some(&command_pos),
        ) {
            Ok(value) => value,
            Err(error) => {
                self.expression_error(&error);
                return Action::None;
            }
        };
        match target {
            Target::Insert(modifier) => {
                let contents = value.register_contents(false);
                self.insert_register_value(
                    &contents.text,
                    contents.kind == super::register::Kind::Line,
                    modifier,
                );
            }
            Target::ReplaceLine => {
                let old_pos = self.command_cursor();
                let at_end = old_pos == self.command.len();
                self.command = value.display().replace(['\n', '\r'], " ");
                self.command_pos = if let Some(pos) = command_pos.get() {
                    self.command
                        .floor_char_boundary(pos.min(self.command.len()))
                } else if at_end {
                    self.command.len()
                } else {
                    self.command
                        .floor_char_boundary(old_pos.min(self.command.len()))
                };
                self.command_prompt.reset();
                self.preview_search(0);
            }
            Target::Execute(count) => {
                register.played_expression = source;
                return self.play_expression_value(value, count, register, resolve_screen);
            }
            Target::Select(count) => {
                register.expression_contents = Some(value.register_contents(true));
                self.selected_register = Some('=');
                self.count = count;
            }
        }
        Action::None
    }

    /// Re-evaluate the source used by the last @= when @@ requests another playback.
    pub(super) fn repeat_expression_macro(
        &mut self,
        count: usize,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        match self.evaluate_expression(&register.played_expression, register) {
            Ok(value) => self.play_expression_value(value, count, register, resolve_screen),
            Err(error) => {
                self.expression_error(&error);
                Action::None
            }
        }
    }

    /// Execute a valid expression value through the register playback parser.
    fn play_expression_value(
        &mut self,
        value: Value,
        count: usize,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if !matches!(value, Value::Integer(_) | Value::String(_)) {
            self.expression_error("A macro expression must return a String or Number");
            return Action::None;
        }
        self.play_keys(
            '=',
            super::register::text_keys(&value.display()),
            count,
            register,
            resolve_screen,
        )
    }

    /// Display a localized error heading and the concrete evaluator failure.
    fn expression_error(&mut self, error: &str) {
        self.message = i18n::format("vim_hint.expression_error", &[("error", &error)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Check typed results, operator precedence and read-only Vim context.
    #[test]
    fn typed_values_and_scoped_context() {
        let mut vim = Vim::new(Document::new("abc 猫\nfour\n"));
        let register = Register::default();
        let eval = |source| vim.evaluate_expression(source, &register).unwrap();
        assert_eq!(eval("5 * (2 + 1) - 2 % 3"), Value::Integer(13));
        assert_eq!(
            eval("1 ? 'it''s ' .. getline(2) : unknown()"),
            Value::String("it's four".into())
        );
        assert_eq!(eval("0 && unknown()"), Value::Integer(0));
        assert_eq!(
            eval("[1, 2, 3][-2:]"),
            Value::List(vec![Value::Integer(2), Value::Integer(3)])
        );
        assert_eq!(
            eval("{'letter': \"\\u732b\"}['letter']"),
            Value::String("猫".into())
        );
        assert_eq!(eval("strlen(getline(1))"), Value::Integer(7));
        assert_eq!(
            eval("join(getline(1, '$'), '|')"),
            Value::String("abc 猫|four".into())
        );
        assert_eq!(eval("'FOO' ==? 'foo'"), Value::Integer(1));
        assert_eq!(eval("line('$')"), Value::Integer(2));
        assert!(
            vim.evaluate_expression("repeat([repeat('a', 500000)], 9000)", &register)
                .is_err()
        );
        assert!(
            vim.evaluate_expression("setline(1, 'other')", &register)
                .is_err()
        );
        assert_eq!(
            vim.evaluate_expression_with(
                "a:type .. ':' .. a:text",
                &register,
                &[
                    ("a:type", Value::String("line".into())),
                    ("a:text", Value::String("changed".into()))
                ]
            ),
            Ok(Value::String("line:changed".into()))
        );
        vim.set_cursor(1);
        assert_eq!(
            vim.evaluate_expression("expand('<cword>')", &register),
            Ok(Value::String("abc".into()))
        );
    }

    /// Exercise Insert and command-line expression prompts, accepting and canceling.
    #[test]
    fn expression_input_preserves_outer_prompt_and_insert_mode() {
        let mut vim = Vim::new(Document::new("abc\n"));
        let mut register = Register::default();
        vim.key("i", &mut register);
        vim.key("Ctrl+r", &mut register);
        vim.key("=", &mut register);
        assert_eq!(vim.prompt(), '=');
        vim.text("1 + 2", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.mode, Mode::Insert);
        assert_eq!(vim.doc.text, "3abc\n");
        vim.key("Escape", &mut register);
        vim.text(":s/abc/", &mut register);
        vim.key("Ctrl+r", &mut register);
        vim.key("=", &mut register);
        vim.text("toupper('cat')", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.command, "s/abc/CAT");
        vim.key("Ctrl+\\", &mut register);
        vim.key("e", &mut register);
        vim.text("getcmdline() .. 's/'", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.command, "s/abc/CATs/");
        vim.key("Ctrl+r", &mut register);
        vim.key("=", &mut register);
        vim.key("Escape", &mut register);
        assert_eq!(vim.command, "s/abc/CATs/");
        vim.key("Escape", &mut register);
        assert_eq!(vim.mode, Mode::Normal);
    }

    /// Run an expression as keystrokes and put a List via Normal-mode register selection.
    #[test]
    fn expression_macro_and_put() {
        let mut vim = Vim::new(Document::new("abcd\nlast\n"));
        let mut register = Register::default();
        vim.text("2@=", &mut register);
        vim.text("'x'", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.text, "cd\nlast\n");
        assert_eq!(register.last_played, Some('='));
        vim.text("\"=", &mut register);
        vim.text("['cat', 'dog']", &mut register);
        vim.key("Enter", &mut register);
        vim.key("p", &mut register);
        assert_eq!(vim.doc.text, "cd\ncat\ndog\nlast\n");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "cd\nlast\n");
    }
}
