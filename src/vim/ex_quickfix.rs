//! Ex spellings shared by quickfix and window-local location lists.

use super::{Action, Vim};

/// A list navigation or display operation executed by the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuickfixCommand {
    /// Open the list, optionally requesting a height in text lines.
    Open(Option<usize>),
    /// Close this list's window.
    Close,
    /// Open a nonempty list or close an empty one, with optional window height.
    Window(Option<usize>),
    /// Revisit the current entry or select its one-based replacement.
    Current(Option<usize>),
    /// Visit the row under the list window's cursor (:.cc or :.ll).
    CurrentLine,
    /// Move forward by the supplied number of entries.
    Next(usize),
    /// Move backward by the supplied number of entries.
    Previous(usize),
    /// Cross file boundaries within the list.
    File {
        /// Use subsequent files when true, earlier files otherwise.
        forward: bool,
        /// Number of file boundaries to cross.
        count: usize,
    },
    /// LazyVim's [q and ]q navigate the open Trouble view or fall back to quickfix.
    Bracket {
        /// Advance when true, go back otherwise.
        forward: bool,
        /// Number of list entries to move.
        count: usize,
    },
    /// Visit the first entry.
    First,
    /// Visit the last entry.
    Last,
    /// Print the inclusive one-based range. Negative addresses count back from the end.
    List {
        /// First entry to print, defaulting to the first list item.
        from: Option<isize>,
        /// Last entry to print, defaulting to the last list item.
        to: Option<isize>,
    },
    /// Print the selected item and the following number of items.
    ListNext(usize),
    /// Select an older list in this quickfix or location list history.
    Older(usize),
    /// Select a newer list in the same history.
    Newer(usize),
    /// Print the available lists or select one by its one-based history number.
    History(Option<usize>),
}

/// Canonical commands and minimum unambiguous lengths from Neovim's Ex command table.
const NAMES: &[(&str, usize)] = &[
    ("copen", 4),
    ("lopen", 3),
    ("cclose", 3),
    ("lclose", 3),
    ("cwindow", 2),
    ("lwindow", 2),
    ("cc", 2),
    ("ll", 2),
    ("cnext", 2),
    ("lnext", 3),
    ("cNext", 2),
    ("lNext", 2),
    ("cprevious", 2),
    ("lprevious", 2),
    ("crewind", 2),
    ("lrewind", 2),
    ("cfirst", 4),
    ("lfirst", 4),
    ("clast", 3),
    ("llast", 3),
    ("clist", 2),
    ("llist", 3),
    ("cnfile", 3),
    ("lnfile", 3),
    ("cpfile", 3),
    ("lpfile", 3),
    ("cNfile", 3),
    ("lNfile", 3),
    ("colder", 3),
    ("lolder", 3),
    ("cnewer", 4),
    ("lnewer", 4),
    ("chistory", 3),
    ("lhistory", 3),
];

/// Return complete command names beginning with the typed prefix.
pub(super) fn candidates(prefix: &str) -> impl Iterator<Item = &'static str> + '_ {
    NAMES
        .iter()
        .map(|&(name, _)| name)
        .filter(move |name| name.starts_with(prefix))
}

impl Vim {
    /// Parse a quickfix or location list command without resolving against source buffer lines.
    pub(super) fn ex_quickfix_command(&mut self, text: &str) -> Option<Action> {
        let text = text.trim();
        let prefix = text
            .bytes()
            .take_while(|b| b.is_ascii_digit() || matches!(b, b'.' | b'$'))
            .count();
        let (address, tail) = text.split_at(prefix);
        let tail = tail.trim_start();
        if !tail.starts_with(['c', 'l']) {
            return None;
        }
        let (verb, tail) = tail.split_at(tail.bytes().take_while(u8::is_ascii_alphabetic).count());
        let &(name, _) = NAMES
            .iter()
            .find(|&&(name, min)| verb.len() >= min && name.starts_with(verb))?;
        let (bang, args) = tail
            .strip_prefix('!')
            .map_or((false, tail), |args| (true, args));
        let command = parse(&name[1..], address.trim(), bang, args.trim());
        Some(match command {
            Some(command) => Action::Quickfix {
                location: name.starts_with('l'),
                command,
            },
            None => {
                self.publish_ex_message(
                    &crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]),
                    true,
                );
                Action::None
            }
        })
    }
}

/// Validate a count or height when present, accepting positive decimal numbers.
fn count(text: &str) -> Option<Option<usize>> {
    if text.is_empty() {
        Some(None)
    } else {
        text.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then_some(())?;
        Some(Some(text.parse().ok().filter(|&n| n > 0)?))
    }
}

/// Translate a canonical suffix and its optional Ex address, bang and arguments.
fn parse(suffix: &str, address: &str, bang: bool, args: &str) -> Option<QuickfixCommand> {
    use QuickfixCommand as Q;
    if suffix == "list" {
        return address.is_empty().then(|| list_range(args)).flatten();
    }
    if suffix == "close" {
        return (!bang && address.is_empty() && args.is_empty()).then_some(Q::Close);
    }
    if matches!(suffix, "c" | "l") && args.is_empty() {
        match address {
            "." => return Some(Q::CurrentLine),
            "$" => return Some(Q::Last),
            _ => {}
        }
    }
    let n = count(args)?.or(count(address)?);
    Some(match suffix {
        "open" if !bang => Q::Open(n),
        "window" if !bang => Q::Window(n),
        "c" | "l" => Q::Current(n),
        "next" => Q::Next(n.unwrap_or(1)),
        "previous" | "Next" => Q::Previous(n.unwrap_or(1)),
        "nfile" | "pfile" | "Nfile" => Q::File {
            forward: suffix == "nfile",
            count: n.unwrap_or(1),
        },
        "first" | "rewind" => n.map_or(Q::First, |n| Q::Current(Some(n))),
        "last" => n.map_or(Q::Last, |n| Q::Current(Some(n))),
        "older" if !bang => Q::Older(n.unwrap_or(1)),
        "newer" if !bang => Q::Newer(n.unwrap_or(1)),
        "history" if !bang => Q::History(n),
        _ => return None,
    })
}

/// Parse the signed endpoints or current-relative form accepted by :clist and :llist.
fn list_range(args: &str) -> Option<QuickfixCommand> {
    if let Some(following) = args.strip_prefix('+') {
        let count = if following.is_empty() {
            1
        } else {
            following.parse().ok()?
        };
        return Some(QuickfixCommand::ListNext(count));
    }
    if args == "," {
        return None;
    }
    let endpoint = |s: &str| -> Option<Option<isize>> {
        match s.trim() {
            "" => Some(None),
            value => Some(Some(value.parse().ok()?)),
        }
    };
    let (first, last) = args.split_once(',').unwrap_or((args, args));
    Some(QuickfixCommand::List {
        from: endpoint(first)?,
        to: endpoint(last)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{QuickfixCommand as Q, *};
    use crate::{document::Document, vim::Register};
    use nanobug_plugin_sdk::DiagnosticSeverity;

    /// Exercise the full Ex dispatcher, including built-ins with overlapping prefixes.
    #[test]
    fn quickfix_ex_names_abbreviations_and_counts() {
        let mut vim = Vim::new(Document::new("one"));
        let mut register = Register::default();
        for &(name, min) in NAMES {
            let short = &name[..min];
            let expected = vim.execute_ex_line(name, &mut register);
            assert!(
                matches!(expected, Action::Quickfix { .. }),
                "{name}: {expected:?}"
            );
            assert_eq!(
                vim.execute_ex_line(short, &mut register),
                expected,
                "{short}"
            );
            assert!(
                vim.command_candidates(short, &register)
                    .unwrap()
                    .1
                    .iter()
                    .any(|item| item == name),
                "{name}"
            );
        }
        for (line, location, command) in [
            ("cn3", false, Q::Next(3)),
            ("3lN!", true, Q::Previous(3)),
            ("2cc! 4", false, Q::Current(Some(4))),
            ("3cfirst", false, Q::Current(Some(3))),
            (".ll", true, Q::CurrentLine),
            ("$cc", false, Q::Last),
            ("5lw", true, Q::Window(Some(5))),
            ("2col", false, Q::Older(2)),
            ("lnew 4", true, Q::Newer(4)),
            ("chi", false, Q::History(None)),
            ("3lhi", true, Q::History(Some(3))),
            ("chistory 2", false, Q::History(Some(2))),
            ("lfir! 2", true, Q::Current(Some(2))),
            (
                "cNf! 3",
                false,
                Q::File {
                    forward: false,
                    count: 3,
                },
            ),
            (
                "lli! -3, -1",
                true,
                Q::List {
                    from: Some(-3),
                    to: Some(-1),
                },
            ),
            (
                "cl 2,",
                false,
                Q::List {
                    from: Some(2),
                    to: None,
                },
            ),
            ("cl +4", false, Q::ListNext(4)),
            (
                "cl -2",
                false,
                Q::List {
                    from: Some(-2),
                    to: Some(-2),
                },
            ),
        ] {
            assert_eq!(
                vim.execute_ex_line(line, &mut register),
                Action::Quickfix { location, command },
                "{line}"
            );
        }
        for bad in ["cn 0", "copen!", "2lclose", "cl 1 3", "ll nope"] {
            assert_eq!(
                vim.execute_ex_line(bad, &mut register),
                Action::None,
                "{bad}"
            );
            assert!(vim.ex_failed, "{bad}");
        }
        for other in ["clo", "lo", "ln", "lc", "!cc", "cq", "cop"] {
            assert!(vim.ex_quickfix_command(other).is_none(), "{other}");
        }
        assert_eq!(
            vim.execute_ex_line("clo", &mut register),
            Action::CloseWindow(false)
        );
        assert_eq!(
            vim.execute_ex_line("lo 3", &mut register),
            Action::SessionExport(crate::vim::SessionExport::LoadView(Some(3)))
        );
        assert_eq!(
            vim.execute_ex_line("!cc", &mut register),
            Action::Shell("cc".into())
        );
        vim.execute_ex_line("1cop 1", &mut register);
        assert_eq!(vim.doc.text, "one\none");
    }

    /// Feed physical-key tokens through the bracket prefix and count machinery.
    #[test]
    fn quickfix_brackets_and_severity_motions_keep_counts() {
        let mut vim = Vim::new(Document::new("one"));
        let mut register = Register::default();
        for (prefix, key, number, location, command) in [
            (
                "[",
                "q",
                None,
                false,
                Q::Bracket {
                    forward: false,
                    count: 1,
                },
            ),
            (
                "]",
                "q",
                Some("3"),
                false,
                Q::Bracket {
                    forward: true,
                    count: 3,
                },
            ),
            ("[", "Q", None, false, Q::First),
            ("]", "L", None, true, Q::Last),
            ("]", "L", Some("3"), true, Q::Current(Some(3))),
            ("[", "l", Some("2"), true, Q::Previous(2)),
            (
                "[",
                "Ctrl+q",
                None,
                false,
                Q::File {
                    forward: false,
                    count: 1,
                },
            ),
            (
                "]",
                "Ctrl+l",
                Some("2"),
                true,
                Q::File {
                    forward: true,
                    count: 2,
                },
            ),
        ] {
            if let Some(number) = number {
                vim.key(number, &mut register);
            }
            vim.key(prefix, &mut register);
            assert_eq!(
                vim.key(key, &mut register),
                Action::Quickfix { location, command },
                "{prefix}{key}"
            );
        }
        for (prefix, key, severity) in [
            ("[", "e", DiagnosticSeverity::Error),
            ("]", "w", DiagnosticSeverity::Warning),
        ] {
            vim.key("3", &mut register);
            vim.key(prefix, &mut register);
            assert_eq!(
                vim.key(key, &mut register),
                Action::BracketDiagnostic {
                    severity: Some(severity),
                    forward: prefix == "]",
                    edge: false,
                    count: 3
                }
            );
        }
    }
}
