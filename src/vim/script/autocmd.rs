//! Workspace-scoped user autocommands and their buffer matching.

use super::split_word;
use crate::vim::{Action, Register, Vim};
use regex::Regex;
use std::{collections::BTreeSet, fmt::Write as _, path::Path};

/// Events emitted by the current native editing and save paths.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScriptEvent {
    BufReadPost,
    BufNewFile,
    BufEnter,
    BufWritePre,
    BufWritePost,
    User,
}

impl ScriptEvent {
    /// Resolve a case-insensitive event spelling and the BufRead/BufWrite aliases.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "bufread" | "bufreadpost" => Some(Self::BufReadPost),
            "bufnewfile" => Some(Self::BufNewFile),
            "bufenter" => Some(Self::BufEnter),
            "bufwrite" | "bufwritepre" => Some(Self::BufWritePre),
            "bufwritepost" => Some(Self::BufWritePost),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    /// Canonical name used when listing autocommands.
    fn name(self) -> &'static str {
        match self {
            Self::BufReadPost => "BufReadPost",
            Self::BufNewFile => "BufNewFile",
            Self::BufEnter => "BufEnter",
            Self::BufWritePre => "BufWritePre",
            Self::BufWritePost => "BufWritePost",
            Self::User => "User",
        }
    }
}

/// An applicable action detached from the registry before a script runs.
pub struct ScriptAutocmd {
    pub text: String,
    pub source: Option<String>,
    pub nested: bool,
}

/// Event handlers and groups shared by the workspace, including startup source files.
#[derive(Default)]
pub(super) struct Registry {
    groups: BTreeSet<String>,
    current: Option<String>,
    entries: Vec<Entry>,
}

struct Entry {
    group: Option<String>,
    event: ScriptEvent,
    pattern: Pattern,
    text: String,
    source: Option<String>,
    nested: bool,
    once: bool,
}

struct Pattern {
    name: String,
    buffer: Option<u64>,
    regex: Option<Regex>,
    full: bool,
}

impl Pattern {
    /// Match a buffer-local definition or a Vim file glob against a native path.
    fn matches(&self, buffer: u64, full: &str, base: &str) -> bool {
        if let Some(number) = self.buffer {
            number == buffer
        } else {
            self.regex
                .as_ref()
                .is_some_and(|regex| regex.is_match(if self.full { full } else { base }))
        }
    }

    /// Compile one glob or resolve a buffer-number pattern against the current buffer.
    fn compile(name: &str, current: u64) -> Result<Self, String> {
        let buffer = if name == "<buffer>" {
            Some(current)
        } else if let Some(number) = name
            .strip_prefix("<buffer=")
            .and_then(|tail| tail.strip_suffix('>'))
        {
            Some(
                number
                    .parse()
                    .map_err(|_| format!("Invalid buffer pattern: {name}"))?,
            )
        } else {
            None
        };
        if let Some(buffer) = buffer {
            return Ok(Self {
                name: format!("<buffer={buffer}>"),
                buffer: Some(buffer),
                regex: None,
                full: false,
            });
        }
        let full = name.contains('/');
        let mut chars = name.chars();
        let mut regex = String::from("^");
        if cfg!(windows) {
            regex.push_str("(?i)");
        }
        let mut groups = 0;
        while let Some(ch) = chars.next() {
            match ch {
                '*' => regex.push_str(".*"),
                '?' => regex.push('.'),
                '{' => {
                    regex.push_str("(?:");
                    groups += 1;
                }
                '}' if groups > 0 => {
                    regex.push(')');
                    groups -= 1;
                }
                ',' if groups > 0 => regex.push('|'),
                '\\' => {
                    if let Some(ch) = chars.next() {
                        regex.push_str(&regex::escape(&ch.to_string()))
                    }
                }
                _ => regex.push_str(&regex::escape(&ch.to_string())),
            }
        }
        regex.push('$');
        let regex = Regex::new(&regex).map_err(|error| error.to_string())?;
        Ok(Self {
            name: name.into(),
            buffer: None,
            regex: Some(regex),
            full,
        })
    }
}

impl crate::vim::InputMappings {
    /// Take applicable actions in registration order, removing each ++once definition.
    pub fn script_autocmds(
        &self,
        event: ScriptEvent,
        group: Option<&str>,
        number: u64,
        path: Option<&Path>,
        explicit: Option<&str>,
    ) -> Vec<ScriptAutocmd> {
        let full = explicit.map(str::to_owned).unwrap_or_else(|| {
            path.map_or(String::new(), |path| {
                path.to_string_lossy().replace('\\', "/")
            })
        });
        let base = full.rsplit('/').next().unwrap_or(&full);
        let state = self.shared_scripts();
        let mut state = state.write().unwrap();
        let mut matched = Vec::new();
        state.autocmds.entries.retain(|entry| {
            if entry.event != event
                || group.is_some_and(|group| entry.group.as_deref() != Some(group))
                || !entry.pattern.matches(number, &full, base)
            {
                return true;
            }
            let text = expand(&entry.text, number, &full);
            matched.push(ScriptAutocmd {
                text,
                source: entry.source.clone(),
                nested: entry.nested,
            });
            !entry.once
        });
        matched
    }
}

impl Vim {
    /// Register, inspect, remove, and explicitly request autocommands.
    pub(super) fn script_autocmd_command(
        &mut self,
        line: &str,
        register: &Register,
    ) -> Option<Action> {
        let (word, args) = split_word(line);
        let name = word.trim_end_matches('!');
        let force = name != word;
        let state = register.input_maps.shared_scripts();
        let mut state = state.write().unwrap();
        let registry = &mut state.autocmds;
        let action = if name.len() >= 3 && "augroup".starts_with(name) {
            registry.group(args, force)
        } else if name.len() >= 2 && "autocmd".starts_with(name) {
            registry.command(
                args,
                force,
                self.doc.buffer_number(),
                register.scripts.source.as_deref(),
            )
        } else if name.len() >= 4
            && ("doautocmd".starts_with(name) || "doautoall".starts_with(name))
            && !force
        {
            registry.trigger(args, "doautoall".starts_with(name) && name.len() >= 7)
        } else if name.len() >= 3 && "noautocmd".starts_with(name) && !force {
            if args.is_empty() {
                Err("Missing command after :noautocmd".into())
            } else {
                Ok(Action::NoAutocmd(args.into()))
            }
        } else {
            return None;
        };
        Some(match action {
            Ok(action) => action,
            Err(error) => {
                self.script_error(&error);
                Action::None
            }
        })
    }
}

impl Registry {
    /// Select, create, list or delete an event group.
    fn group(&mut self, name: &str, force: bool) -> Result<Action, String> {
        if name.is_empty() {
            return Ok(Action::ShowListing(
                self.groups.iter().cloned().collect::<Vec<_>>().join("\n"),
            ));
        }
        if name.split_whitespace().count() != 1 {
            return Err("Expected a single autocommand group".into());
        }
        if force {
            if !self.groups.remove(name) {
                return Err(format!("Unknown autocommand group: {name}"));
            }
            self.entries
                .retain(|entry| entry.group.as_deref() != Some(name));
            if self.current.as_deref() == Some(name) {
                self.current = None;
            }
        } else if name.eq_ignore_ascii_case("END") {
            self.current = None;
        } else {
            self.groups.insert(name.into());
            self.current = Some(name.into());
        }
        Ok(Action::None)
    }

    /// Parse optional group, event, glob, flags and command; manage its definition or listing.
    fn command(
        &mut self,
        args: &str,
        force: bool,
        number: u64,
        source: Option<&str>,
    ) -> Result<Action, String> {
        let (first, remaining) = split_word(args);
        let (group, args) = if self.groups.contains(first) {
            (Some(first), remaining)
        } else {
            (None, args)
        };
        let (names, remaining) = split_word(args);
        let (pattern, mut text) = split_word(remaining);
        let events = events(names)?;
        let mut nested = false;
        let mut once = false;
        loop {
            let (flag, tail) = split_word(text);
            match flag {
                "++nested" | "nested" => nested = true,
                "++once" => once = true,
                _ => break,
            }
            text = tail;
        }
        let group_key = group.or(self.current.as_deref()).map(str::to_owned);
        let patterns = split_patterns(pattern)
            .into_iter()
            .map(|part| Pattern::compile(part, number))
            .collect::<Result<Vec<_>, _>>()?;
        let matches = |entry: &Entry, listing: bool| {
            (listing && group.is_none()
                || entry.group.as_deref() == if listing { group } else { group_key.as_deref() })
                && (events.is_empty() || events.contains(&entry.event))
                && (patterns.is_empty()
                    || patterns
                        .iter()
                        .any(|pattern| pattern.name == entry.pattern.name))
        };
        if force {
            self.entries.retain(|entry| !matches(entry, false));
        }
        if text.is_empty() {
            if force {
                return Ok(Action::None);
            }
            let mut result = String::from("--- Autocommands ---\n");
            for entry in self.entries.iter().filter(|entry| matches(entry, true)) {
                let _ = writeln!(
                    result,
                    "{} {}  {}  {}",
                    entry.group.as_deref().unwrap_or(""),
                    entry.event.name(),
                    entry.pattern.name,
                    entry.text
                );
            }
            return Ok(Action::ShowListing(result));
        }
        if events.is_empty() || patterns.is_empty() {
            return Err("Expected event and pattern for an autocommand".into());
        }
        for event in events {
            for pattern in &patterns {
                self.entries.push(Entry {
                    group: group_key.clone(),
                    event,
                    pattern: Pattern::compile(&pattern.name, number)?,
                    text: text.into(),
                    source: source.map(str::to_owned),
                    nested,
                    once,
                });
            }
        }
        Ok(Action::None)
    }

    /// Resolve the event and optional group and match argument for a manual trigger.
    fn trigger(&self, args: &str, all: bool) -> Result<Action, String> {
        let args = args
            .strip_prefix("<nomodeline>")
            .map_or(args, str::trim_start);
        let (first, tail) = split_word(args);
        let (group, args) = if self.groups.contains(first) {
            (Some(first), tail)
        } else {
            (None, args)
        };
        let (event, filename) = split_word(args);
        if ScriptEvent::parse(event).is_none() {
            return Err(format!("Unknown autocommand event: {event}"));
        }
        Ok(Action::Autocmd {
            event: event.into(),
            pattern: (!filename.is_empty()).then(|| filename.into()),
            group: group.map(str::to_owned),
            all,
        })
    }
}

/// Parse a comma-separated list of supported events or the filter wildcard.
fn events(text: &str) -> Result<Vec<ScriptEvent>, String> {
    if text.is_empty() || text == "*" {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|name| {
            ScriptEvent::parse(name).ok_or_else(|| format!("Unknown autocommand event: {name}"))
        })
        .collect()
}

/// Split pattern alternatives outside glob brace groups and escape sequences.
fn split_patterns(text: &str) -> Vec<&str> {
    let mut start = 0;
    let mut nesting = 0usize;
    let mut split = Vec::new();
    let mut chars = text.char_indices();
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            '{' => nesting += 1,
            '}' => nesting = nesting.saturating_sub(1),
            ',' if nesting == 0 => {
                split.push(&text[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    if start < text.len() {
        split.push(&text[start..]);
    }
    split
}

/// Expand event context placeholders without interpreting inserted filenames again.
fn expand(text: &str, number: u64, path: &str) -> String {
    let number = number.to_string();
    let mut output = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('<') {
        output.push_str(before);
        let Some((key, tail)) = after.split_once('>') else {
            output.push('<');
            output.push_str(after);
            return output;
        };
        let value = match key {
            "abuf" => number.as_str(),
            "afile" | "amatch" => path,
            _ => {
                output.push('<');
                rest = after;
                continue;
            }
        };
        output.push_str(value);
        rest = tail;
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Match glob patterns, drain once-only entries, and retain group-scoped definitions.
    #[test]
    fn autocommand_registry_matches_and_consumes_handlers_in_order() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        for line in [
            "augroup Scope",
            "autocmd BufRead *.txt ++once echo <abuf>",
            "autocmd BufReadPost *.{txt,md} ++nested echo 2",
            "augroup END",
        ] {
            vim.execute_ex_line(line, &mut register);
            assert!(!vim.ex_failed(), "{}", vim.message);
        }
        let number = vim.doc.buffer_number();
        let collect = || {
            register.input_maps.script_autocmds(
                ScriptEvent::BufReadPost,
                Some("Scope"),
                number,
                Some(Path::new("dir/data.txt")),
                None,
            )
        };
        let handlers = collect();
        assert_eq!(handlers.len(), 2);
        assert_eq!(handlers[0].text, format!("echo {number}"));
        assert!(!handlers[0].nested && handlers[1].nested);
        assert_eq!(collect().len(), 1);
        vim.execute_ex_line("augroup! Scope", &mut register);
        assert!(
            register
                .input_maps
                .script_autocmds(
                    ScriptEvent::BufReadPost,
                    None,
                    number,
                    None,
                    Some("file.md")
                )
                .is_empty()
        );
    }
}
