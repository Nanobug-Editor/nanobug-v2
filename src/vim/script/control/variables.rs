//! Local Ex variables, binding locks, and expression scope resolution.

use super::{ScriptProgram, split_fields};
use crate::vim::{Register, Vim, expression::Value};
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
