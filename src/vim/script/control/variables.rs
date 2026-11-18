//! Local Ex variables, binding locks, and expression scope resolution.

use super::{ScriptProgram, split_fields};
use crate::vim::{
    Action, Register, Vim,
    ex_redirect::{RedirectCommand, Redirection},
    expression::Value,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

/// A variable and its immutability at the binding level.
pub(super) struct Binding {
    pub(super) value: Value,
    locked: bool,
    constant: bool,
}

impl Binding {
    /// Construct a writable binding.
    pub(super) fn mutable(value: Value) -> Self {
        Self {
            value,
            locked: false,
            constant: false,
        }
    }

    /// Construct an immutable binding, such as a function argument.
    pub(super) fn constant(value: Value) -> Self {
        Self {
            value,
            locked: true,
            constant: true,
        }
    }
}

/// Persistent variables for one native script interpreter.
#[derive(Default)]
pub(in crate::vim) struct Variables {
    global: BTreeMap<String, Binding>,
    source: BTreeMap<String, BTreeMap<String, Binding>>,
    buffer: BTreeMap<u64, BTreeMap<String, Binding>>,
}

/// Native variables scoped to a Tabpage and shared by its splits.
#[derive(Clone, Default)]
pub struct ScriptTabScope(Arc<RwLock<BTreeMap<String, Binding>>>);

/// Pane-local variables and a reference to the current Tabpage's variables.
#[derive(Default)]
pub(in crate::vim::script) struct WindowVariables {
    window: BTreeMap<String, Binding>,
    pub(in crate::vim::script) tab: ScriptTabScope,
}

impl Variables {
    /// Resolve variables that are also available in plain Ex or Insert-mode expressions.
    pub(in crate::vim) fn lookup(&self, buffer: u64, scope: &str, key: &str) -> Option<Value> {
        let store = match scope {
            "g" => &self.global,
            "b" => self.buffer.get(&buffer)?,
            _ => return None,
        };
        store.get(key).map(|binding| binding.value.clone())
    }
}

impl WindowVariables {
    /// Read a window-local or current Tabpage variable.
    pub(in crate::vim::script) fn lookup(&self, scope: &str, key: &str) -> Option<Value> {
        match scope {
            "w" => self.window.get(key).map(|binding| binding.value.clone()),
            "t" => self
                .tab
                .0
                .read()
                .unwrap()
                .get(key)
                .map(|binding| binding.value.clone()),
            _ => None,
        }
    }
}

impl ScriptProgram {
    /// Complete one redirect before beginning another or stopping capture.
    pub(in crate::vim) fn finish_redirection(
        &mut self,
        vim: &Vim,
        register: &mut Register,
    ) -> Result<(), String> {
        match register.input_maps.take_redirection() {
            Some(Redirection::Clipboard(text)) => {
                register.input_maps.finish_redirect_clipboard(text)
            }
            Some(Redirection::Variable(name, text)) => {
                self.assign_redirected(vim, register, &name, Some(&text))?;
            }
            Some(Redirection::File(_, Some(error))) => return Err(error),
            Some(Redirection::File(_, None) | Redirection::Register(_)) | None => {}
        }
        Ok(())
    }

    /// Validate the destination before any subsequent Ex output can be recorded.
    pub(in crate::vim) fn start_redirection(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        command: RedirectCommand<'_>,
    ) -> Result<Action, String> {
        match command {
            RedirectCommand::End => {}
            RedirectCommand::File(path, mode) => {
                return Ok(Action::RedirectFile {
                    path: path.into(),
                    mode,
                });
            }
            RedirectCommand::Register('+' | '*', append) => {
                return Ok(Action::RedirectClipboard { append });
            }
            RedirectCommand::Register(name, append) => {
                let slot = register.redirect_to_register(name, append);
                register
                    .input_maps
                    .start_redirection(Redirection::Register(slot));
            }
            RedirectCommand::Variable(name, append) => {
                self.assign_redirected(vim, register, name, append.then_some(""))?;
                let name = if name
                    .split(['.', '['])
                    .next()
                    .is_some_and(|base| base.contains(':'))
                {
                    name.to_owned()
                } else {
                    format!("{}:{name}", if self.function { "l" } else { "g" })
                };
                register
                    .input_maps
                    .start_redirection(Redirection::Variable(name, String::new()));
            }
        }
        Ok(Action::None)
    }

    /// Resolve a variable or indexed item afresh when validating or completing a redirection.
    fn assign_redirected(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        name: &str,
        append: Option<&str>,
    ) -> Result<(), String> {
        let base_end = name.find(['.', '[']).unwrap_or(name.len());
        let (base, mut tail) = name.split_at(base_end);
        let base = base.trim_end();
        let mut indices = Vec::new();
        while !tail.is_empty() {
            if let Some(field) = tail.strip_prefix('.') {
                let end = field
                    .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                    .unwrap_or(field.len());
                if end == 0 {
                    return Err(format!("Expected a dictionary key in {name}"));
                }
                indices.push(Value::String(field[..end].to_owned()));
                tail = &field[end..];
            } else if let Some(index) = tail.strip_prefix('[') {
                let end = end_of_index(index).ok_or_else(|| format!("Missing ] in {name}"))?;
                indices.push(self.evaluate(vim, &index[..end], register)?);
                tail = &index[end + 1..];
            } else {
                return Err(format!("Invalid redirection variable: {name}"));
            }
            tail = tail.trim_start();
        }
        if indices.is_empty() && append.is_none() {
            return self.assign(vim, register, base, Value::String(String::new()), false);
        }
        self.with_store(vim, register, base, |store, key| {
            let binding = store
                .get_mut(key)
                .ok_or_else(|| format!("Undefined variable: {base}"))?;
            if binding.locked {
                return Err(format!("Variable is locked: {base}"));
            }
            let mut value = &mut binding.value;
            let length = indices.len();
            for (offset, index) in indices.into_iter().enumerate() {
                value = match (value, index) {
                    (Value::Dictionary(values), index @ (Value::String(_) | Value::Integer(_))) => {
                        let key = index.display();
                        if append.is_none() && offset + 1 == length {
                            values
                                .entry(key)
                                .or_insert_with(|| Value::String(String::new()))
                        } else {
                            values
                                .get_mut(&key)
                                .ok_or_else(|| format!("Unknown dictionary key: {key}"))?
                        }
                    }
                    (Value::List(values), Value::Integer(index)) => {
                        let index = if index < 0 {
                            values.len() as i64 + index
                        } else {
                            index
                        };
                        values
                            .get_mut(index as usize)
                            .ok_or_else(|| format!("List index out of range: {index}"))?
                    }
                    _ => return Err(format!("Invalid indexed redirection variable: {name}")),
                };
            }
            if let Some(text) = append {
                let Value::String(value) = value else {
                    return Err(format!("Expected a String variable: {name}"));
                };
                value.push_str(text);
            } else {
                *value = Value::String(String::new());
            }
            Ok(())
        })
    }

    /// Evaluate an expression with the locals and source-specific variables of this frame.
    pub(super) fn evaluate(
        &self,
        vim: &Vim,
        text: &str,
        register: &Register,
    ) -> Result<Value, String> {
        let bindings = self.bindings(register);
        let pairs = bindings
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect::<Vec<_>>();
        vim.evaluate_expression_with(text, register, &pairs)
    }

    /// Collect the frame-specific bindings that override variables in the register bank.
    pub(in crate::vim::script) fn bindings(&self, register: &Register) -> BTreeMap<String, Value> {
        let mut visible = BTreeMap::new();
        let shared = register.input_maps.shared_scripts();
        let shared = shared.read().unwrap();
        if let Some(vars) = self
            .source
            .as_ref()
            .and_then(|key| shared.variables.source.get(key))
        {
            for (key, binding) in vars {
                visible.insert(format!("s:{key}"), binding.value.clone());
            }
        }
        for (key, binding) in &self.locals {
            visible.insert(key.clone(), binding.value.clone());
            if !key.contains(':') {
                visible.insert(format!("l:{key}"), binding.value.clone());
            }
        }
        visible
    }

    /// Assign a loop item to a variable or a destructuring list of variables.
    pub(super) fn assign_for(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        names: &str,
        value: Value,
    ) -> Result<(), String> {
        self.assign_item(vim, register, names, value, false)
    }

    /// Recognize variable statements, leaving native option assignments to the Ex dispatcher.
    pub(in crate::vim::script) fn variable_command(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        word: &str,
        args: &str,
    ) -> Result<bool, String> {
        let name = word.trim_end_matches('!');
        let force = word != name;
        match name {
            "let" | "const" | "cons" if !force => {
                let Some((target, expression)) = args.split_once('=') else {
                    return if name == "let" {
                        Ok(false)
                    } else {
                        Err("Missing assignment value".into())
                    };
                };
                let target = target.trim();
                if name == "let" && target == "v:errmsg" {
                    let value = self.evaluate(vim, expression.trim(), register)?;
                    register.input_maps.remember_ex_error(&value.display());
                    return Ok(true);
                }
                if target.starts_with('&') || matches!(target, "mapleader" | "g:mapleader") {
                    return if name == "let" {
                        Ok(false)
                    } else {
                        Err("Expected a variable for :const".into())
                    };
                }
                let operation = target.chars().last().filter(|ch| "+-*/%.".contains(*ch));
                let (target, value) = if let Some(operation) = operation {
                    let target = target.trim_end_matches(operation).trim_end();
                    let operation = if operation == '.' {
                        "..".into()
                    } else {
                        operation.to_string()
                    };
                    let value = self.evaluate(
                        vim,
                        &format!("{target} {operation} ({expression})"),
                        register,
                    )?;
                    (target, value)
                } else {
                    (target, self.evaluate(vim, expression.trim(), register)?)
                };
                self.assign_item(vim, register, target, value, name != "let")?;
            }
            "unlet" | "unl" | "unle" => {
                if args.is_empty() {
                    return Err("Expected at least one variable".into());
                }
                for name in args.split_whitespace() {
                    self.with_store(vim, register, name, |vars, key| {
                        if vars.get(key).is_some_and(|value| value.constant) {
                            return Err(format!("Cannot remove constant variable: {name}"));
                        }
                        if vars.remove(key).is_none() && !force {
                            return Err(format!("Undefined variable: {name}"));
                        }
                        Ok(())
                    })?;
                }
            }
            "lockvar" | "lockv" | "lockva" | "unlockvar" | "unlo" | "unloc" | "unlockv"
            | "unlockva" => {
                let lock = name.starts_with("lock");
                let mut names = args.split_whitespace().peekable();
                let depth = names.peek().and_then(|name| name.parse::<usize>().ok());
                if depth.is_some() {
                    names.next();
                }
                if names.peek().is_none() {
                    return Err("Expected at least one variable".into());
                }
                for name in names {
                    self.with_store(vim, register, name, |vars, key| {
                        let entry = vars
                            .get_mut(key)
                            .ok_or_else(|| format!("Undefined variable: {name}"))?;
                        if entry.constant && !lock {
                            return Err(format!("Cannot unlock constant variable: {name}"));
                        }
                        if depth != Some(0) || force {
                            entry.locked = lock;
                        }
                        Ok(())
                    })?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Assign a scalar or unpack a list, honoring locked bindings.
    fn assign_item(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        names: &str,
        value: Value,
        constant: bool,
    ) -> Result<(), String> {
        if let Some(names) = names
            .strip_prefix('[')
            .and_then(|name| name.strip_suffix(']'))
        {
            let Value::List(mut items) = value else {
                return Err("Expected a List for variable unpacking".into());
            };
            let (head, tail) = names
                .split_once(';')
                .map_or((names, None), |(head, tail)| (head, Some(tail.trim())));
            let names = split_fields(head, ',');
            if items.len() < names.len() || tail.is_none() && items.len() != names.len() {
                return Err("Wrong number of list items to unpack".into());
            }
            let rest = items.split_off(names.len());
            for (name, item) in names.into_iter().zip(items) {
                self.assign(vim, register, name, item, constant)?;
            }
            if let Some(tail) = tail {
                self.assign(vim, register, tail, Value::List(rest), constant)?;
            }
            Ok(())
        } else {
            self.assign(vim, register, names, value, constant)
        }
    }

    /// Create or replace a mutable, named binding.
    fn assign(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        name: &str,
        value: Value,
        constant: bool,
    ) -> Result<(), String> {
        self.with_store(vim, register, name, |store, key| {
            if store
                .get(key)
                .is_some_and(|entry| entry.locked || entry.constant || constant)
            {
                return Err(format!("Variable is locked or already defined: {name}"));
            }
            store.insert(
                key.to_owned(),
                if constant {
                    Binding::constant(value)
                } else {
                    Binding::mutable(value)
                },
            );
            Ok(())
        })
    }

    /// Select the variable scope and return its key without the scope prefix.
    fn with_store<T>(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        name: &str,
        apply: impl FnOnce(&mut BTreeMap<String, Binding>, &str) -> Result<T, String>,
    ) -> Result<T, String> {
        let (scope, key) = name
            .split_once(':')
            .unwrap_or((if self.function { "l" } else { "g" }, name));
        if !key
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!("Invalid variable name: {name}"));
        }
        match scope {
            "l" if self.function => apply(&mut self.locals, key),
            "w" => apply(&mut register.scripts.variables.window, key),
            "t" => apply(&mut register.scripts.variables.tab.0.write().unwrap(), key),
            "g" | "s" | "b" => {
                let shared = register.input_maps.shared_scripts();
                let mut shared = shared.write().unwrap();
                let variables = &mut shared.variables;
                let store = match scope {
                    "g" => &mut variables.global,
                    "s" => {
                        let source = self
                            .source
                            .as_ref()
                            .ok_or("Script variables require :source")?;
                        variables.source.entry(source.clone()).or_default()
                    }
                    _ => variables.buffer.entry(vim.doc.buffer_number()).or_default(),
                };
                apply(store, key)
            }
            "a" | "v" => Err(format!("Read-only variable: {name}")),
            _ => Err(format!("Invalid variable scope: {name}")),
        }
    }
}

/// Find the closing bracket of an index, ignoring quoted text and nested indices.
fn end_of_index(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if let Some(end) = quote {
            if end == '"' && ch == '\\'
                || end == '\'' && ch == end && chars.peek().is_some_and(|&(_, next)| next == end)
            {
                chars.next();
            } else if ch == end {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '[' => depth += 1,
            ']' if depth == 0 => return Some(at),
            ']' => depth -= 1,
            _ => {}
        }
    }
    None
}
