//! Definitions and expansion for native user-created Ex commands.

use super::{split_fields, split_word};
use crate::{
    i18n,
    vim::{Action, Register, Vim},
};
use std::{collections::BTreeMap, fmt::Write as _};

/// Definitions shared across one Ex interpreter, with separate buffer-local entries.
#[derive(Default)]
pub(super) struct UserCommands {
    global: BTreeMap<String, Definition>,
    buffers: BTreeMap<u64, BTreeMap<String, Definition>>,
}

/// Parsed arguments and body of a declared Ex command.
struct Definition {
    body: String,
    source: Option<String>,
    nargs: Nargs,
    bang: bool,
    bar: bool,
    register: bool,
    address: Address,
    keep_script: bool,
    completion: Option<Completion>,
}

#[derive(Default)]
enum Nargs {
    #[default]
    Zero,
    Optional,
    Required,
    Multiple,
    Many,
}

#[derive(Default)]
enum Address {
    #[default]
    None,
    Current,
    Entire,
    Count(usize),
}

#[derive(Clone, Copy)]
pub(in crate::vim) enum Completion {
    File,
    Directory,
    Command,
    Option,
}

/// Effective call arguments available to placeholders in a user command.
struct Arguments<'a> {
    raw: &'a str,
    nargs: &'a Nargs,
    bang: bool,
    count: usize,
    range: usize,
    first: usize,
    last: usize,
    register: &'a str,
}

impl Vim {
    /// List non-shadowed names for the current buffer with the typed prefix.
    pub fn user_ex_candidates(&self, prefix: &str, register: &Register) -> Vec<String> {
        let shared = register.input_maps.shared_scripts();
        let shared = shared.read().unwrap();
        let commands = &shared.user_commands;
        commands
            .global
            .keys()
            .chain(
                commands
                    .buffers
                    .get(&self.doc.buffer_number())
                    .into_iter()
                    .flat_map(BTreeMap::keys),
            )
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Read the completion form of a custom command after its name has been typed.
    pub(in crate::vim) fn user_ex_completion(
        &self,
        word: &str,
        register: &Register,
    ) -> Option<Completion> {
        let shared = register.input_maps.shared_scripts();
        let shared = shared.read().unwrap();
        let commands = &shared.user_commands;
        commands
            .find(self.doc.buffer_number(), word.trim_end_matches('!'))
            .ok()
            .flatten()?
            .completion
    }

    /// Find and expand command definitions or a user invocation in normal Ex dispatch.
    pub(super) fn user_ex_command(
        &mut self,
        line: &str,
        register: &mut Register,
    ) -> Option<Action> {
        let (initial, tail) = split_word(line);
        let plain = initial.trim_end_matches('!');
        let force = plain != initial;
        let number = self.doc.buffer_number();
        let result = if plain.len() >= 3 && "command".starts_with(plain) {
            self.define_ex_command(tail, force, register)
        } else if plain.len() >= 4 && "delcommand".starts_with(plain) && !force {
            let (local, name) = scope_prefix(tail);
            register
                .input_maps
                .shared_scripts()
                .write()
                .unwrap()
                .user_commands
                .remove(number, name.trim(), local)
                .map(|()| Action::None)
        } else if plain.len() >= 4 && "comclear".starts_with(plain) && initial == plain {
            let (local, rest) = scope_prefix(tail);
            if rest.trim().is_empty() {
                let shared = register.input_maps.shared_scripts();
                let commands = &mut shared.write().unwrap().user_commands;
                if !local {
                    commands.global.clear();
                }
                commands.buffers.remove(&number);
                Ok(Action::None)
            } else {
                Err("Unexpected argument to :comclear".into())
            }
        } else {
            let (range, name_source) = self.ex_range(line)?;
            let (name, args) = split_word(name_source);
            let word = name.trim_end_matches('!');
            if !word.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
                return None;
            }
            let shared = register.input_maps.shared_scripts();
            let shared = shared.read().unwrap();
            let definition = match shared.user_commands.find(number, word) {
                Ok(Some(definition)) => definition,
                Ok(None) => return None,
                Err(error) => {
                    self.script_error(&error);
                    return Some(Action::None);
                }
            };
            let bang = name != word;
            self.expand_ex_command(definition, register, line, name_source, args, bang, range)
        };
        Some(match result {
            Ok(action) => action,
            Err(error) => {
                self.script_error(&error);
                Action::None
            }
        })
    }

    /// Define a command or display the requested prefix from the visible command registry.
    fn define_ex_command(
        &mut self,
        arguments: &str,
        force: bool,
        register: &mut Register,
    ) -> Result<Action, String> {
        let mut rest = arguments;
        let mut local = false;
        let mut definition = Definition {
            body: String::new(),
            source: register.scripts.source.clone(),
            nargs: Nargs::Zero,
            bang: false,
            bar: false,
            register: false,
            address: Address::None,
            keep_script: false,
            completion: None,
        };
        while let (flag, tail) = split_word(rest)
            && flag.starts_with('-')
        {
            rest = tail;
            if flag == "-buffer" {
                local = true;
                continue;
            }
            let (flag, value) = flag
                .split_once('=')
                .map_or((flag, None), |(name, value)| (name, Some(value)));
            match (flag, value) {
                ("-bang", None) => definition.bang = true,
                ("-bar", None) => definition.bar = true,
                ("-register", None) => definition.register = true,
                ("-keepscript", None) => definition.keep_script = true,
                ("-range", None) => definition.address = Address::Current,
                ("-range", Some("%")) => definition.address = Address::Entire,
                ("-range", Some(count)) => definition.address = Address::Count(read_count(count)?),
                ("-count", count) => {
                    definition.address =
                        Address::Count(count.map(read_count).transpose()?.unwrap_or(0))
                }
                ("-addr", Some("lines")) => {}
                ("-nargs", Some(value)) => {
                    definition.nargs = match value {
                        "0" => Nargs::Zero,
                        "1" => Nargs::Required,
                        "?" => Nargs::Optional,
                        "*" => Nargs::Multiple,
                        "+" => Nargs::Many,
                        _ => return Err(format!("Invalid -nargs value: {value}")),
                    }
                }
                ("-complete", Some(value)) => {
                    definition.completion = Some(match value {
                        "file" => Completion::File,
                        "dir" => Completion::Directory,
                        "command" => Completion::Command,
                        "option" => Completion::Option,
                        _ => return Err(format!("Unknown user command completion: {value}")),
                    })
                }
                _ => return Err(format!("Invalid user command attribute: {flag}")),
            }
        }
        let (name, body) = split_word(rest);
        let shared = register.input_maps.shared_scripts();
        let commands = &mut shared.write().unwrap().user_commands;
        let number = self.doc.buffer_number();
        if body.is_empty() {
            let mut listing = String::new();
            let prefix = name;
            let local_commands = commands.buffers.get(&number);
            for (name, definition) in local_commands
                .into_iter()
                .flat_map(|map| map.iter())
                .chain(commands.global.iter().filter(|(name, _)| {
                    !local && !local_commands.is_some_and(|map| map.contains_key(*name))
                }))
                .filter(|(name, _)| name.starts_with(prefix))
            {
                let _ = writeln!(listing, "{name:<20} {}", definition.body);
            }
            if listing.is_empty() {
                self.message = i18n::text("vim_g.no_user_commands").into();
                return Ok(Action::None);
            }
            return Ok(Action::ShowListing(listing));
        }
        if !valid_name(name) {
            return Err(format!("Invalid user command name: {name}"));
        }
        if definition.completion.is_some() && matches!(definition.nargs, Nargs::Zero) {
            return Err("-complete requires a command that accepts arguments".into());
        }
        let definitions = if local {
            commands.buffers.entry(number).or_default()
        } else {
            &mut commands.global
        };
        if !force && definitions.contains_key(name) {
            return Err(format!("User command already exists: {name}"));
        }
        definition.body = body.into();
        definitions.insert(name.into(), definition);
        Ok(Action::None)
    }

    /// Expand invocation arguments after validating the attributes of the definition.
    fn expand_ex_command(
        &mut self,
        definition: &Definition,
        register: &Register,
        line: &str,
        body: &str,
        mut args: &str,
        bang: bool,
        range: Option<std::ops::Range<usize>>,
    ) -> Result<Action, String> {
        if bang && !definition.bang {
            return Err("User command does not accept !".into());
        }
        if range.is_some() && matches!(definition.address, Address::None) {
            return Err("User command does not accept an address".into());
        }
        let current = self.doc.location(self.doc.cursor).0 + 1;
        let mut count = match definition.address {
            Address::Count(count) => count,
            _ => current,
        };
        let addresses = if range.is_some() {
            if line[..line.len() - body.len()].contains([',', ';', '%']) {
                2
            } else {
                1
            }
        } else {
            0
        };
        let (first, last) = match range {
            Some(range) => {
                count = range.end;
                (range.start + 1, range.end)
            }
            None if matches!(definition.address, Address::Entire) => (1, self.line_count()),
            None => (current, current),
        };
        if matches!(definition.address, Address::Count(_))
            && let (token, rest) = split_word(args)
            && !token.is_empty()
            && token.bytes().all(|byte| byte.is_ascii_digit())
        {
            count = read_count(token)?;
            args = rest;
        }
        let mut reg = "";
        if definition.register {
            let (token, rest) = split_word(args);
            let name = token.trim_start_matches('"');
            if name.len() == 1
                && name
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || "-*+_.:%#=/".contains(ch))
            {
                reg = name;
                args = rest;
            }
        }
        let mut extra = None;
        if definition.bar {
            if let Some((head, tail)) = unescaped_bar(args) {
                args = head.trim_end();
                extra = Some(tail.trim_start());
            }
        }
        match definition.nargs {
            Nargs::Zero if !args.is_empty() => {
                return Err("User command does not accept arguments".into());
            }
            Nargs::Required | Nargs::Many if args.is_empty() => {
                return Err("User command requires an argument".into());
            }
            _ => {}
        }
        let arguments = Arguments {
            raw: args,
            nargs: &definition.nargs,
            bang,
            count,
            range: addresses,
            first,
            last,
            register: reg,
        };
        let mut text = arguments.expand(&definition.body);
        if let Some(extra) = extra.filter(|extra| !extra.is_empty()) {
            text.push_str(" | ");
            text.push_str(extra);
        }
        let source = if definition.keep_script {
            register.scripts.source.clone()
        } else {
            definition.source.clone()
        };
        Ok(Action::UserEx { text, source })
    }
}

impl UserCommands {
    /// Resolve an exact command or unique abbreviation, preferring the buffer-local name.
    fn find(&self, buffer: u64, prefix: &str) -> Result<Option<&Definition>, String> {
        let local = self.buffers.get(&buffer);
        if let Some(exact) = local
            .and_then(|commands| commands.get(prefix))
            .or_else(|| self.global.get(prefix))
        {
            return Ok(Some(exact));
        }
        let mut partial =
            local
                .into_iter()
                .flat_map(|commands| commands.iter())
                .chain(self.global.iter().filter(|(name, _)| {
                    !local.is_some_and(|commands| commands.contains_key(*name))
                }))
                .filter(|(name, _)| name.starts_with(prefix))
                .map(|(_, definition)| definition);
        let found = partial.next();
        if partial.next().is_some() {
            return Err(format!("Ambiguous user command: {prefix}"));
        }
        Ok(found)
    }

    /// Remove the local definition first unless the command explicitly requests another scope.
    fn remove(&mut self, buffer: u64, name: &str, local: bool) -> Result<(), String> {
        let removed = self
            .buffers
            .get_mut(&buffer)
            .and_then(|commands| commands.remove(name));
        if removed.is_some() || !local && self.global.remove(name).is_some() {
            Ok(())
        } else {
            Err(format!("Unknown user command: {name}"))
        }
    }
}

impl Arguments<'_> {
    /// Replace recognized angle-bracket placeholders once, preserving literal body text.
    fn expand(&self, body: &str) -> String {
        let mut output = String::new();
        let mut rest = body;
        while let Some((before, after)) = rest.split_once('<') {
            output.push_str(before);
            let Some((item, tail)) = after.split_once('>') else {
                output.push('<');
                output.push_str(after);
                return output;
            };
            let value = match item.to_ascii_lowercase().as_str() {
                "args" => self.raw.into(),
                "q-args" => quote(self.raw),
                "f-args" if matches!(self.nargs, Nargs::Optional | Nargs::Required) => {
                    if self.raw.is_empty() {
                        String::new()
                    } else {
                        quote(self.raw)
                    }
                }
                "f-args" => split_arguments(self.raw)
                    .into_iter()
                    .map(|argument| quote(&argument))
                    .collect::<Vec<_>>()
                    .join(", "),
                "bang" => if self.bang { "!" } else { "" }.into(),
                "count" => self.count.to_string(),
                "line1" => self.first.to_string(),
                "line2" => self.last.to_string(),
                "range" => self.range.to_string(),
                "reg" | "register" => self.register.into(),
                "lt" => "<".into(),
                _ => {
                    output.push('<');
                    rest = after;
                    continue;
                }
            };
            output.push_str(&value);
            rest = tail;
        }
        output.push_str(rest);
        output
    }
}

/// Separate an optional -buffer attribute from the main argument.
fn scope_prefix(input: &str) -> (bool, &str) {
    let (word, tail) = split_word(input);
    if word == "-buffer" {
        (true, tail)
    } else {
        (false, input)
    }
}

/// Require an uppercase first letter, reserving the upstream built-in aliases.
fn valid_name(word: &str) -> bool {
    word.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && word.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !matches!(word, "X" | "Next")
}

/// Parse an unsigned Ex command count.
fn read_count(input: &str) -> Result<usize, String> {
    input
        .parse()
        .map_err(|_| format!("Invalid user command count: {input}"))
}

/// Quote an argument as a Vim double-quoted String.
fn quote(input: &str) -> String {
    let body = input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{body}\"")
}

/// Split arguments on unescaped spaces, interpreting escaped spaces and backslashes.
fn split_arguments(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else if ch == '\\'
            && chars
                .peek()
                .is_some_and(|next| next.is_whitespace() || *next == '\\')
        {
            current.push(chars.next().expect("peeked escaped argument"));
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Locate a pipeline separator that is neither quoted nor escaped.
fn unescaped_bar(input: &str) -> Option<(&str, &str)> {
    let first = split_fields(input, '|').into_iter().next()?;
    input
        .get(first.len()..)?
        .trim_start()
        .strip_prefix('|')
        .map(|tail| (first, tail))
}
