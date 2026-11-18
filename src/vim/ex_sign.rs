//! Parse native Ex sign commands before dispatching buffer operations to the workspace.

use super::{Action, Register, SignDefinitionUpdate, Vim};
use crate::{document::SignGroup, i18n};
use std::path::PathBuf;

/// Full names offered by Ex completion.
pub(super) const NAMES: &[&str] = &["sign"];

const SUBCOMMANDS: &[&str] = &["define", "undefine", "list", "place", "unplace", "jump"];
const DEFINITION_FIELDS: &[&str] = &[
    "text=",
    "icon=",
    "texthl=",
    "linehl=",
    "numhl=",
    "culhl=",
    "priority=",
];

/// An explicit buffer addressed by an Ex sign operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignTarget {
    /// An already known file. Embedded spaces are part of the filename.
    File(String),
    /// Vim buffer number exposed by `:buffers`.
    Buffer(u64),
}

/// Select one or more placements for an unplace operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignId {
    /// Exact placement identifier inside each selected group.
    One(u64),
    /// All placements within the selected scope.
    All,
    /// The first matching placement on the active cursor row.
    Cursor,
}

/// A sign definition or a request affecting placed signs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignCommand {
    /// Define a sign or merge the fields explicitly supplied by the command.
    Define {
        name: String,
        definition: SignDefinitionUpdate,
    },
    /// Remove a definition by name.
    Undefine { name: String },
    /// Print all definitions or just one named definition.
    List { name: Option<String> },
    /// Place or redefine a sign in an explicit target or the current buffer.
    Place {
        id: u64,
        name: String,
        /// One-based line address. None changes an existing sign in place.
        line: Option<usize>,
        group: Option<String>,
        priority: Option<i32>,
        target: Option<SignTarget>,
    },
    /// List placements in a target buffer or in all buffers if no target is supplied.
    ListPlaced {
        group: SignGroup,
        target: Option<SignTarget>,
    },
    /// Remove the requested placements. Explicit IDs without a target span all buffers.
    Unplace {
        id: SignId,
        group: SignGroup,
        target: Option<SignTarget>,
    },
    /// Jump to the requested sign in the current or explicitly targeted buffer.
    Jump {
        id: u64,
        group: Option<String>,
        target: Option<SignTarget>,
    },
}

impl Vim {
    /// Recognize the `:sig[n]` spelling and produce an action for a valid operation.
    pub(super) fn ex_sign_command(&mut self, text: &str) -> Option<Action> {
        let (verb, args) = split_word(text);
        if !is_sign(verb.trim_end_matches('!')) {
            return None;
        }
        match (!verb.ends_with('!')).then(|| parse(args)).flatten() {
            Some(command) => Some(Action::Sign(command)),
            None => {
                self.publish_ex_message(
                    &i18n::format("editor.invalid_ex_argument", &[("argument", &text)]),
                    true,
                );
                Some(Action::None)
            }
        }
    }
}

/// Recognize the command before reading its subcommand and arguments.
fn is_sign(name: &str) -> bool {
    matches!(name, "sig" | "sign")
}

/// Protect the full sign argument from the Ex pipeline scanner, including text and file bars.
pub(super) fn protected_prefix(command: &str) -> usize {
    let (_, _, body) = super::ex_window_modifiers::command_modifiers(command);
    if is_sign(split_word(body).0) {
        command.len()
    } else {
        0
    }
}

/// Read an Ex subcommand and validate the form of its requested operation.
fn parse(args: &str) -> Option<SignCommand> {
    let (subcommand, args) = split_word(args);
    let (name, tail) = split_word(args);
    match subcommand {
        "define" => Some(SignCommand::Define {
            name: sign_name(name)?,
            definition: parse_definition(tail)?,
        }),
        "undefine" if tail.is_empty() => Some(SignCommand::Undefine {
            name: sign_name(name)?,
        }),
        "list" if tail.is_empty() => Some(SignCommand::List {
            name: if name.is_empty() {
                None
            } else {
                Some(sign_name(name)?)
            },
        }),
        "place" | "unplace" | "jump" => PlacementArgs::parse(args)?.into_command(subcommand),
        _ => None,
    }
}

/// Set only the fields present in this definition or update.
fn parse_definition(mut args: &str) -> Option<SignDefinitionUpdate> {
    let mut definition = SignDefinitionUpdate::default();
    while !args.is_empty() {
        let (field, rest) = take_definition_field(args);
        let (key, value) = field.split_once('=')?;
        match key {
            "text" => {
                let mut chars = value.chars();
                let mut text = String::new();
                while let Some(ch) = chars.next() {
                    text.push(if ch == '\\' {
                        chars.next().unwrap_or(ch)
                    } else {
                        ch
                    });
                }
                definition.text = Some(text);
            }
            "icon" => {
                let mut chars = value.chars().peekable();
                let mut path = String::new();
                while let Some(ch) = chars.next() {
                    if ch == '\\' && chars.peek().is_some_and(|ch| ch.is_ascii_whitespace()) {
                        path.push(chars.next()?);
                    } else {
                        path.push(ch);
                    }
                }
                definition.icon = Some(PathBuf::from(path));
            }
            "texthl" => definition.texthl = Some(value.into()),
            "linehl" => definition.linehl = Some(value.into()),
            "numhl" => definition.numhl = Some(value.into()),
            "culhl" | "cursorlinehl" => definition.culhl = Some(value.into()),
            "priority" => definition.priority = Some(value.parse().ok()?),
            _ => return None,
        }
        args = rest;
    }
    Some(definition)
}

/// Keep backslash-escaped spaces within a definition field.
fn take_definition_field(args: &str) -> (&str, &str) {
    let mut chars = args.char_indices();
    while let Some((at, ch)) = chars.next() {
        if ch == '\\' {
            chars.next();
        } else if ch.is_ascii_whitespace() {
            return (&args[..at], args[at..].trim_ascii_start());
        }
    }
    (args, "")
}

/// Normalize numeric names without limiting their length to a machine integer.
fn sign_name(name: &str) -> Option<String> {
    if name.is_empty() || name.chars().any(char::is_control) {
        None
    } else if name.bytes().all(|byte| byte.is_ascii_digit()) {
        let trimmed = name.trim_start_matches('0');
        Some(if trimmed.is_empty() { "0" } else { trimmed }.into())
    } else if name.as_bytes()[0].is_ascii_digit() {
        None
    } else {
        Some(name.into())
    }
}

/// Fields common to place, unplace and jump before applying their subcommand-specific rules.
#[derive(Default)]
struct PlacementArgs {
    id: Option<SignId>,
    line: Option<usize>,
    name: Option<String>,
    group: Option<String>,
    priority: Option<i32>,
    target: Option<SignTarget>,
}

impl PlacementArgs {
    /// Read the optional ID, keyed fields, and a final literal filename or buffer number.
    fn parse(mut args: &str) -> Option<Self> {
        let mut values = Self::default();
        let (first, rest) = split_word(args);
        if !first.is_empty() && first.bytes().all(|byte| byte.is_ascii_digit()) {
            values.id = Some(SignId::One(first.parse().ok()?));
            args = rest;
        }
        while !args.is_empty() {
            if let Some(file) = args.strip_prefix("file=") {
                let file = file.trim_ascii_end();
                if file.is_empty() {
                    return None;
                }
                values.target = Some(SignTarget::File(file.into()));
                break;
            }
            let (field, rest) = split_word(args);
            if field == "*" && values.id.is_none() {
                values.id = Some(SignId::All);
                args = rest;
                continue;
            }
            let (key, value) = field.split_once('=')?;
            match key {
                "line" => values.line = Some(value.parse::<usize>().ok().filter(|line| *line > 0)?),
                "name" => values.name = Some(sign_name(value)?),
                "group" if !value.is_empty() => values.group = Some(value.into()),
                "priority" => values.priority = Some(value.parse().ok()?),
                "buffer" if rest.is_empty() => {
                    values.target = Some(SignTarget::Buffer(
                        value.parse::<u64>().ok().filter(|n| *n > 0)?,
                    ));
                }
                _ => return None,
            }
            args = rest;
        }
        Some(values)
    }

    /// Keep the documented distinctions between a missing selector, a wildcard and an exact ID.
    fn into_command(self, subcommand: &str) -> Option<SignCommand> {
        let Self {
            id,
            line,
            name,
            group,
            priority,
            target,
        } = self;
        let selected_group = || match group.as_deref() {
            None => SignGroup::Global,
            Some("*") => SignGroup::All,
            Some(name) => SignGroup::Named(name.into()),
        };
        match (subcommand, id, name, line) {
            ("place", None | Some(SignId::One(0)), None, None) if priority.is_none() => {
                Some(SignCommand::ListPlaced {
                    group: selected_group(),
                    target,
                })
            }
            ("place", Some(SignId::One(id)), Some(name), line)
                if id > 0 && group.as_deref().is_none_or(|group| !group.starts_with('*')) =>
            {
                Some(SignCommand::Place {
                    id,
                    name,
                    line,
                    group,
                    priority,
                    target,
                })
            }
            ("unplace", id, None, None)
                if priority.is_none() && (id.is_some() || target.is_none()) =>
            {
                let id = match id {
                    None => SignId::Cursor,
                    Some(SignId::One(0)) => SignId::All,
                    Some(id) => id,
                };
                Some(SignCommand::Unplace {
                    id,
                    group: selected_group(),
                    target,
                })
            }
            ("jump", Some(SignId::One(id)), None, None)
                if id > 0
                    && priority.is_none()
                    && group.as_deref().is_none_or(|group| !group.starts_with('*')) =>
            {
                Some(SignCommand::Jump { id, group, target })
            }
            _ => None,
        }
    }
}

/// Split at command whitespace without interpreting quotes, path separators or comments.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_ascii_start();
    text.split_once(|ch: char| ch.is_ascii_whitespace())
        .map_or((text, ""), |(word, tail)| (word, tail.trim_ascii_start()))
}

/// Complete a subcommand, field, sign name, highlight group or literal file path.
pub(super) fn completion<'a>(
    name: &str,
    arguments: &'a str,
    register: &Register,
    files: impl FnOnce(&str) -> Vec<String>,
) -> Option<(&'a str, Vec<String>)> {
    if !is_sign(name) {
        return None;
    }
    let args = arguments.trim_ascii_start();
    if !args.chars().any(|ch| ch.is_ascii_whitespace()) {
        return Some((args, matching(SUBCOMMANDS, args)));
    }
    let (verb, tail) = split_word(args);
    let stem = tail
        .rsplit(|ch: char| ch.is_ascii_whitespace())
        .next()
        .unwrap_or_default();
    let names = |prefix: &str| {
        register
            .input_maps
            .sign_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .filter(|name| name.starts_with(prefix))
            .collect()
    };
    if matches!(verb, "place" | "unplace" | "jump") {
        if let Some((_, prefix)) = tail.match_indices("file=").find_map(|(at, word)| {
            (at == 0 || tail.as_bytes()[at - 1].is_ascii_whitespace())
                .then_some((word, &tail[at + word.len()..]))
        }) {
            return Some((prefix, files(prefix)));
        }
        if verb == "place"
            && let Some(prefix) = stem.strip_prefix("name=")
        {
            return Some((prefix, names(prefix)));
        }
        if let Some(prefix) = stem.strip_prefix("group=") {
            return Some((prefix, matching(&["*"], prefix)));
        }
        let fields = if verb == "place" {
            &["file=", "buffer=", "group=", "line=", "name=", "priority="][..]
        } else {
            &["file=", "buffer=", "group="][..]
        };
        return Some((stem, matching(fields, stem)));
    }
    if !tail.chars().any(|ch| ch.is_ascii_whitespace()) {
        return Some((stem, names(stem)));
    }
    if verb == "define" {
        if let Some((key, prefix)) = stem.split_once('=')
            && matches!(
                key,
                "texthl" | "linehl" | "numhl" | "culhl" | "cursorlinehl"
            )
        {
            return Some((prefix, register.input_maps.highlight_groups(prefix)));
        }
        return Some((stem, matching(DEFINITION_FIELDS, stem)));
    }
    None
}

/// Filter a static list by the command fragment currently being typed.
fn matching(values: &[&str], prefix: &str) -> Vec<String> {
    values
        .iter()
        .filter(|value| value.starts_with(prefix))
        .map(|value| (*value).into())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Verify that each explicit field is decoded while omitted fields remain absent.
    #[test]
    fn sign_parse_definition_fields_and_escaped_windows_path() {
        let sign = parse(
            r"define 0003 text=\>! icon=C:\work\two\ words.bmp texthl=Search linehl=Normal numhl=ErrorMsg culhl=Visual priority=20",
        );
        assert_eq!(
            sign,
            Some(SignCommand::Define {
                name: "3".into(),
                definition: SignDefinitionUpdate {
                    text: Some(">!".into()),
                    icon: Some(PathBuf::from(r"C:\work\two words.bmp")),
                    texthl: Some("Search".into()),
                    linehl: Some("Normal".into()),
                    numhl: Some("ErrorMsg".into()),
                    culhl: Some("Visual".into()),
                    priority: Some(20),
                },
            })
        );
        let Some(SignCommand::Define { definition, .. }) = parse(r"define test text=\ ") else {
            panic!("expected a sign definition");
        };
        assert_eq!(definition.text.as_deref(), Some(" "));
        assert_eq!(definition.icon, None);
        assert_eq!(
            parse("undefine 004"),
            Some(SignCommand::Undefine { name: "4".into() })
        );
        assert_eq!(parse("list"), Some(SignCommand::List { name: None }));
    }

    /// Distinguish placing a sign, changing its definition and listing placements.
    #[test]
    fn sign_parse_scopes_places_and_jumps() {
        assert_eq!(
            parse(
                r"place 3 line=20 group=work name=Demo priority=30 file=C:\demo projects\main.rs"
            ),
            Some(SignCommand::Place {
                id: 3,
                name: "Demo".into(),
                line: Some(20),
                group: Some("work".into()),
                priority: Some(30),
                target: Some(SignTarget::File(r"C:\demo projects\main.rs".into())),
            })
        );
        assert_eq!(
            parse("place 3 name=New buffer=12"),
            Some(SignCommand::Place {
                id: 3,
                name: "New".into(),
                line: None,
                group: None,
                priority: None,
                target: Some(SignTarget::Buffer(12)),
            })
        );
        assert_eq!(
            parse("place group=*"),
            Some(SignCommand::ListPlaced {
                group: SignGroup::All,
                target: None,
            })
        );
        assert_eq!(
            parse("jump 7 group=work"),
            Some(SignCommand::Jump {
                id: 7,
                group: Some("work".into()),
                target: None
            })
        );
    }

    /// Scope wildcards and reject conflicting or misplaced arguments.
    #[test]
    fn sign_parse_removal_selectors_and_invalid_forms() {
        assert_eq!(
            parse("unplace"),
            Some(SignCommand::Unplace {
                id: SignId::Cursor,
                group: SignGroup::Global,
                target: None,
            })
        );
        assert_eq!(
            parse("unplace * group=lint buffer=2"),
            Some(SignCommand::Unplace {
                id: SignId::All,
                group: SignGroup::Named("lint".into()),
                target: Some(SignTarget::Buffer(2)),
            })
        );
        assert_eq!(
            parse("unplace 2 group=*"),
            Some(SignCommand::Unplace {
                id: SignId::One(2),
                group: SignGroup::All,
                target: None,
            })
        );
        for bad in [
            "",
            "define",
            "define flag unknown=value",
            "list flag another",
            "place 4 line=1",
            "place 4 name=flag group=*",
            "place 4 name=flag line=0",
            "place 4 name=flag line=1 buffer=2 group=late",
            "place name=flag",
            "place file=",
            "unplace * name=flag",
            "unplace 2 *",
            "jump 2 line=1",
        ] {
            assert!(
                parse(bad).is_none(),
                "accepted invalid sign arguments: {bad}"
            );
        }
    }

    /// Exercise abbreviation, completion and the Ex boundary without using Neovim.
    #[test]
    fn sign_dispatch_and_completion_preserve_literal_arguments() {
        let mut vim = Vim::new(Document::new("one"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("sig list", &mut register),
            Action::Sign(SignCommand::List { name: None })
        );
        vim.execute_ex_line("sign!", &mut register);
        assert!(vim.ex_failed());
        assert_eq!(
            vim.command_candidates("sign pl", &register),
            Some((5, vec!["place".into()]))
        );
        assert_eq!(
            vim.command_candidates("sign define Demo text", &register),
            Some((17, vec!["text=".into(), "texthl=".into()]))
        );
        assert_eq!(
            vim.source_statements("echo 'a' | silent sign define Pipe text=||"),
            vec!["echo 'a'", "silent sign define Pipe text=||"]
        );
        assert_eq!(
            vim.source_statements("sign place file=pipe | filename"),
            vec!["sign place file=pipe | filename"]
        );
    }
}
