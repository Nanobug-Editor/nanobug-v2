//! Native bindings for the diagnostic commands used in Neovim and LazyVim mappings.

use super::{Action, Vim};
use nanobug_plugin_sdk::DiagnosticSeverity;

/// A diagnostic operation requested from the command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCommand {
    /// Show messages spanning the caret's logical line.
    Line,
    /// Enable or disable diagnostic display across the editor.
    Enable(bool),
    /// Copy current diagnostics into a navigation list.
    Export {
        /// Target the source window's location list when true, or the project's quickfix list.
        location: bool,
        /// Focus the result window after replacing its contents.
        open: bool,
        /// Restrict the snapshot to one severity when supplied.
        severity: Option<DiagnosticSeverity>,
        /// Neovim uses the title to find and update an earlier diagnostic Quickfix list.
        title: Option<String>,
    },
    /// Change visibility of a Trouble-compatible list.
    View(DiagnosticList, ListVisibility),
}

/// Available Trouble modes, including its current-buffer filter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticList {
    Project,
    Buffer,
    Quickfix,
    Location,
}

/// Visibility change requested for a result window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListVisibility {
    Open,
    Close,
    Toggle,
}

impl Vim {
    /// Parse supported diagnostic bindings; leave other Lua or user commands to Ex.
    pub(super) fn ex_diagnostic_command(&mut self, text: &str) -> Option<Action> {
        let text = text.trim();
        let (name, tail) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let result = match name {
            "Trouble" => trouble(tail),
            "lua" => diagnostic_call(tail.trim().strip_prefix("vim.diagnostic.")?),
            _ => return None,
        };
        Some(result.unwrap_or_else(|| {
            self.publish_ex_message(
                &crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]),
                true,
            );
            Action::None
        }))
    }
}

/// Decode a Trouble mode, an optional action and its current-buffer filter.
fn trouble(args: &str) -> Option<Action> {
    let mut words = args.split_whitespace();
    let mut list = match words.next()? {
        "diagnostics" => DiagnosticList::Project,
        "qflist" | "quickfix" => DiagnosticList::Quickfix,
        "loclist" => DiagnosticList::Location,
        _ => return None,
    };
    let mut visibility = None;
    for word in words {
        if word == "filter.buf=0" && list == DiagnosticList::Project {
            list = DiagnosticList::Buffer;
            continue;
        }
        if visibility.is_some() {
            return None;
        }
        visibility = Some(match word {
            "open" => ListVisibility::Open,
            "close" => ListVisibility::Close,
            "toggle" => ListVisibility::Toggle,
            _ => return None,
        });
    }
    Some(Action::Diagnostic(DiagnosticCommand::View(
        list,
        visibility.unwrap_or(ListVisibility::Open),
    )))
}

/// Decode the literal arguments used by the supported vim.diagnostic entry points.
fn diagnostic_call(call: &str) -> Option<Action> {
    let name_end = call.find(|c: char| c.is_whitespace() || c == '(' || c == '{')?;
    let name = &call[..name_end];
    let tail = call[name_end..].trim();
    let args = if let Some(body) = tail.strip_prefix('(') {
        body.strip_suffix(')')?.trim()
    } else {
        tail.starts_with('{').then_some(tail)?
    };
    if name == "enable" {
        return Some(Action::Diagnostic(DiagnosticCommand::Enable(match args {
            "" | "true" => true,
            "false" => false,
            _ => return None,
        })));
    }
    let fields = if args.is_empty() {
        ""
    } else {
        let body = args.strip_prefix('{')?.strip_suffix('}')?.trim();
        body.strip_suffix([',', ';']).unwrap_or(body).trim_end()
    };
    let mut open = true;
    let mut severity = None;
    let mut title = None;
    let mut count: Option<isize> = None;
    // A quoted diagnostic title can itself contain Lua's table field separators.
    let mut quote = None;
    let mut escaped = false;
    let separator = |ch| {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
        } else if matches!(ch, '"' | '\'') {
            quote = Some(ch);
        } else {
            return matches!(ch, ',' | ';');
        }
        false
    };
    for field in fields.split(separator).filter(|_| !fields.is_empty()) {
        let (key, value) = field.split_once('=')?;
        let value = value.trim();
        match (name, key.trim()) {
            ("setqflist" | "setloclist", "open") => open = value.parse().ok()?,
            ("setqflist" | "setloclist", "title") => title = Some(quoted_title(value)?),
            ("setqflist" | "setloclist" | "jump", "severity") => {
                severity = Some(match value {
                    "vim.diagnostic.severity.ERROR" | "1" => DiagnosticSeverity::Error,
                    "vim.diagnostic.severity.WARN" | "2" => DiagnosticSeverity::Warning,
                    "vim.diagnostic.severity.INFO" | "3" => DiagnosticSeverity::Information,
                    "vim.diagnostic.severity.HINT" | "4" => DiagnosticSeverity::Hint,
                    _ => return None,
                })
            }
            ("jump", "count") => count = Some(value.parse().ok()?),
            ("open_float", "scope") if matches!(value, "'line'" | "\"line\"" | "'l'" | "\"l\"") => {
            }
            _ => return None,
        }
    }
    Some(match name {
        "open_float" => Action::Diagnostic(DiagnosticCommand::Line),
        "setqflist" | "setloclist" => Action::Diagnostic(DiagnosticCommand::Export {
            location: name == "setloclist",
            open,
            severity,
            title,
        }),
        "jump" => {
            let count = count.filter(|n| *n != 0)?;
            Action::BracketDiagnostic {
                forward: count > 0,
                edge: false,
                count: count.unsigned_abs(),
                severity,
            }
        }
        _ => return None,
    })
}

/// Read a single quoted Lua title, including escaped quotes and backslashes.
fn quoted_title(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let delimiter = chars.next().filter(|c| matches!(c, '\'' | '"'))?;
    let mut title = String::new();
    while let Some(ch) = chars.next() {
        if ch == delimiter {
            return chars.as_str().is_empty().then_some(title);
        }
        title.push(if ch == '\\' {
            match chars.next()? {
                c @ ('\'' | '"' | '\\') => c,
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            }
        } else {
            ch
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Exercise the actual Ex dispatcher, including mixed-case Trouble and literal Lua options.
    #[test]
    fn diagnostic_commands_validate_options_and_preserve_list_scope() {
        let mut vim = Vim::new(Document::new("demo"));
        let mut register = Register::default();
        let mut run = |line| vim.execute_ex_line(line, &mut register);
        assert_eq!(
            run(":Trouble diagnostics toggle filter.buf=0"),
            Action::Diagnostic(DiagnosticCommand::View(
                DiagnosticList::Buffer,
                ListVisibility::Toggle
            ))
        );
        assert_eq!(
            run(
                ":lua vim.diagnostic.setqflist({ title = 'Rust, errors'; severity = vim.diagnostic.severity.ERROR, open = false })"
            ),
            Action::Diagnostic(DiagnosticCommand::Export {
                location: false,
                open: false,
                severity: Some(DiagnosticSeverity::Error),
                title: Some("Rust, errors".into())
            })
        );
        assert_eq!(
            run(":lua vim.diagnostic.setloclist()"),
            Action::Diagnostic(DiagnosticCommand::Export {
                location: true,
                open: true,
                severity: None,
                title: None
            })
        );
        assert_eq!(
            run(":lua vim.diagnostic.jump { count=-2, severity=vim.diagnostic.severity.WARN }"),
            Action::BracketDiagnostic {
                forward: false,
                edge: false,
                count: 2,
                severity: Some(DiagnosticSeverity::Warning)
            }
        );
        assert_eq!(
            run(":Trouble diagnostics toggle filter.buf=whatever"),
            Action::None
        );
        assert!(vim.ex_failed());
    }
}

/// Complete the supported diagnostic command argument under the prompt caret.
pub(super) fn completion<'a>(name: &str, args: &'a str) -> Option<(&'a str, Vec<String>)> {
    let (prefix, words): (&str, &[&str]) = match name {
        "lua" => (
            args,
            &[
                "vim.diagnostic.open_float()",
                "vim.diagnostic.setqflist()",
                "vim.diagnostic.setloclist()",
                "vim.diagnostic.enable(true)",
                "vim.diagnostic.enable(false)",
                "vim.diagnostic.jump({count=1})",
            ],
        ),
        "Trouble" => {
            if let Some((mode, tail)) = args.split_once(char::is_whitespace) {
                let prefix = tail
                    .rsplit_once(char::is_whitespace)
                    .map_or(tail, |(_, last)| last);
                let choices: &[&str] = match mode {
                    "diagnostics" => &["open", "close", "toggle", "filter.buf=0"],
                    "qflist" | "quickfix" | "loclist" => &["open", "close", "toggle"],
                    _ => &[],
                };
                (prefix, choices)
            } else {
                (args, &["diagnostics", "qflist", "loclist"])
            }
        }
        _ => return None,
    };
    Some((
        prefix,
        words
            .iter()
            .filter(|s| s.starts_with(prefix))
            .map(|s| (*s).to_owned())
            .collect(),
    ))
}
