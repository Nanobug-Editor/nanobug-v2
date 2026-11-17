//! Ex actions that inspect or change files and Session-owned navigation.

use super::{Action, Vim};

pub(super) mod search;

/// Native spellings accepted by this command family and offered by completion.
pub(super) const NAMES: &[&str] = &[
    "cd",
    "chdir",
    "lcd",
    "lchdir",
    "tcd",
    "tchdir",
    "drop",
    "view",
    "sview",
    "checktime",
    "ex",
    "file",
    "find",
    "pwd",
    "sfind",
    "tabfind",
    "bmodified",
    "brewind",
    "sargument",
    "sbfirst",
    "sblast",
    "sbmodified",
    "sbnext",
    "sbNext",
    "sbprevious",
    "sbrewind",
    "sbuffer",
    "sfirst",
    "slast",
    "snext",
    "sNext",
    "sprevious",
    "srewind",
    "wnext",
    "wNext",
    "wprevious",
];

/// Where to show the file located through 'path'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileDestination {
    Current,
    Split,
    Tabpage,
}

/// Movement within the Session's argument list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileArgument {
    Current,
    First,
    Last,
    Next,
    Previous,
}

/// Selection within the Session's shared buffer list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileBuffer {
    Exact,
    First,
    Last,
    Next,
    Previous,
    Modified,
}

/// Behavior selected by an Ex file or buffer command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOperation {
    CheckTime,
    Edit,
    Name,
    Find(FileDestination),
    WorkingDirectory,
    GlobalDirectory,
    TabDirectory,
    WindowDirectory,
    Drop,
    View { split: bool },
    Buffer { target: FileBuffer, split: bool },
    Argument { target: FileArgument, write: bool },
}

/// Host request parsed from an Ex word, its optional address, and arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct FileCommand {
    pub operation: FileOperation,
    pub count: Option<usize>,
    pub force: bool,
    pub arguments: String,
}

impl Vim {
    /// Recognize file commands without treating their buffer or argument addresses as text lines.
    pub(super) fn ex_file_command(&mut self, text: &str) -> Option<Action> {
        let word = text.trim_start();
        let prefix = word.bytes().take_while(u8::is_ascii_digit).count();
        let count = if prefix == 0 {
            None
        } else {
            word[..prefix].parse::<usize>().ok()
        };
        let word = word[prefix..].trim_start();
        let length = word.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (name, arguments) = word.split_at(length);
        let operation = parse_operation(name)?;
        let (force, arguments) = arguments
            .strip_prefix('!')
            .map_or((false, arguments), |tail| (true, tail));
        let arguments = arguments.trim();
        let permits_force = matches!(
            operation,
            FileOperation::Name
                | FileOperation::Edit
                | FileOperation::Find(_)
                | FileOperation::GlobalDirectory
                | FileOperation::TabDirectory
                | FileOperation::WindowDirectory
                | FileOperation::Drop
                | FileOperation::View { .. }
                | FileOperation::Buffer { split: false, .. }
                | FileOperation::Argument { .. }
        );
        let address_valid = match operation {
            FileOperation::Name => count.is_none() || count == Some(0) && arguments.is_empty(),
            FileOperation::WorkingDirectory => count.is_none() && arguments.is_empty(),
            FileOperation::Edit
            | FileOperation::GlobalDirectory
            | FileOperation::TabDirectory
            | FileOperation::WindowDirectory
            | FileOperation::Drop
            | FileOperation::View { split: false } => count.is_none(),
            FileOperation::Find(FileDestination::Tabpage) => true,
            _ => count != Some(0),
        };
        if !address_valid || (prefix > 0 && count.is_none()) || (force && !permits_force) {
            self.ex_failed = true;
            self.message =
                crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
            return Some(Action::None);
        }
        Some(Action::Files(FileCommand {
            operation,
            count,
            force,
            arguments: arguments.to_owned(),
        }))
    }
}

/// Match native full names and their command-specific minimum abbreviations.
fn parse_operation(name: &str) -> Option<FileOperation> {
    use FileArgument as A;
    use FileBuffer as B;
    use FileDestination as D;
    use FileOperation::*;
    let matches = |full: &str, min| name.len() >= min && full.starts_with(name);
    let buffer = |target, split| Buffer { target, split };
    let arg = |target, write| Argument { target, write };
    Some(match name {
        "ex" => Edit,
        "pwd" | "pw" => WorkingDirectory,
        "cd" => GlobalDirectory,
        _ if matches("chdir", 3) => GlobalDirectory,
        _ if matches("lcd", 2) || matches("lchdir", 3) => WindowDirectory,
        _ if matches("tcd", 2) || matches("tchdir", 3) => TabDirectory,
        _ if matches("drop", 2) => Drop,
        _ if matches("view", 3) => View { split: false },
        _ if matches("sview", 2) => View { split: true },
        _ if matches("file", 1) => Name,
        _ if matches("find", 3) => Find(D::Current),
        _ if matches("sfind", 2) => Find(D::Split),
        _ if matches("tabfind", 4) => Find(D::Tabpage),
        _ if matches("checktime", 6) => CheckTime,
        _ if matches("bmodified", 2) => buffer(B::Modified, false),
        _ if matches("brewind", 2) => buffer(B::First, false),
        _ if matches("sbuffer", 2) => buffer(B::Exact, true),
        _ if matches("sbfirst", 3) || matches("sbrewind", 3) => buffer(B::First, true),
        _ if matches("sblast", 3) => buffer(B::Last, true),
        _ if matches("sbmodified", 3) => buffer(B::Modified, true),
        _ if matches("sbnext", 3) => buffer(B::Next, true),
        _ if matches("sbNext", 3) || matches("sbprevious", 3) => buffer(B::Previous, true),
        _ if matches("sargument", 2) => arg(A::Current, false),
        _ if matches("sfirst", 4) || matches("srewind", 3) => arg(A::First, false),
        _ if matches("slast", 3) => arg(A::Last, false),
        _ if matches("snext", 2) => arg(A::Next, false),
        _ if matches("sNext", 2) || matches("sprevious", 3) => arg(A::Previous, false),
        _ if matches("wnext", 2) => arg(A::Next, true),
        _ if matches("wNext", 2) || matches("wprevious", 2) => arg(A::Previous, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Every catalogued spelling reaches the typed workspace action.
    #[test]
    fn ex_files_command_names_resolve_to_host_actions() {
        let mut vim = Vim::new(Document::new("line\n"));
        let mut register = Register::default();
        for name in NAMES {
            assert!(
                matches!(vim.execute_ex_line(name, &mut register), Action::Files(_)),
                "{name}"
            );
        }
        assert_eq!(
            vim.execute_ex_line("12sbNext 3", &mut register),
            Action::Files(FileCommand {
                operation: FileOperation::Buffer {
                    target: FileBuffer::Previous,
                    split: true
                },
                count: Some(12),
                force: false,
                arguments: "3".into(),
            })
        );
    }
}
