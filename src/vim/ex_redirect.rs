//! Capture Ex output into a file, register, clipboard or script variable.

use super::{Action, Register, Vim};
use std::{fs::File, io, io::Write, path::Path};

pub(super) const NAMES: &[&str] = &["redir"];

/// How a file redirection opens its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectMode {
    /// Create a file, rejecting an existing destination.
    Create,
    /// Create or truncate a file.
    Replace,
    /// Open or create a file for append.
    Append,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RedirectCommand<'a> {
    End,
    File(&'a str, RedirectMode),
    Register(char, bool),
    Variable(&'a str, bool),
}

/// The active destination, kept open until :redir END or another :redir.
pub(super) enum Redirection {
    File(File, Option<String>),
    Register(super::register::RedirectedRegister),
    Clipboard(String),
    Variable(String, String),
}

impl Redirection {
    pub(super) fn file(path: &Path, mode: RedirectMode) -> io::Result<Self> {
        let mut options = File::options();
        match mode {
            RedirectMode::Create => options.write(true).create_new(true),
            RedirectMode::Replace => options.write(true).create(true).truncate(true),
            RedirectMode::Append => options.append(true).create(true),
        };
        Ok(Self::File(options.open(path)?, None))
    }

    /// Echo and listing messages start on a new line. :echon continues the last line.
    pub(super) fn write(&mut self, text: &str, newline: bool) {
        match self {
            Self::File(file, error) if error.is_none() => {
                let result = (if newline {
                    file.write_all(b"\n")
                } else {
                    Ok(())
                })
                .and_then(|()| file.write_all(text.as_bytes()));
                *error = result.err().map(|error| error.to_string());
            }
            Self::File(..) => {}
            Self::Register(slot) => {
                let mut value = slot.lock().unwrap();
                if newline {
                    value.text.push('\n');
                }
                value.text.push_str(text);
                value.kind = super::register::Kind::Character;
            }
            Self::Clipboard(output) | Self::Variable(_, output) => {
                if newline {
                    output.push('\n');
                }
                output.push_str(text);
            }
        }
    }
}

/// Decode the optional whitespace-delimited redirection command.
pub(super) fn parse(line: &str) -> Option<Result<RedirectCommand<'_>, &'static str>> {
    let line = line.trim();
    let (word, arguments) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let (name, force) = word
        .strip_suffix('!')
        .map_or((word, false), |name| (name, true));
    if !matches!(name, "redi" | "redir") {
        return None;
    }
    let arguments = arguments.trim();
    let command = match arguments {
        args if args.eq_ignore_ascii_case("END") => Ok(RedirectCommand::End),
        args if args.starts_with("=>") => {
            let (name, append) = destination(args, "=>");
            if name.is_empty() {
                Err("Expected a variable after :redir =>")
            } else {
                Ok(RedirectCommand::Variable(name, append))
            }
        }
        args if args.starts_with('>') => {
            let (path, append) = destination(args, ">");
            if path.is_empty() {
                Err("Expected a filename after :redir >")
            } else {
                let mode = match (append, force) {
                    (true, _) => RedirectMode::Append,
                    (false, true) => RedirectMode::Replace,
                    _ => RedirectMode::Create,
                };
                Ok(RedirectCommand::File(path, mode))
            }
        }
        args if args.starts_with('@') => {
            let mut chars = args[1..].chars();
            match (chars.next(), chars.as_str()) {
                (
                    Some(name @ ('a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '"' | '+' | '*')),
                    suffix @ ("" | ">" | ">>"),
                ) => Ok(RedirectCommand::Register(
                    name,
                    suffix == ">>" || name.is_ascii_uppercase(),
                )),
                _ => Err("Expected a writable register after :redir @"),
            }
        }
        _ => Err("Expected > file, @register, => variable or END after :redir"),
    };
    Some(command)
}

fn destination<'a>(arguments: &'a str, operator: &str) -> (&'a str, bool) {
    let tail = &arguments[operator.len()..];
    tail.strip_prefix('>')
        .map_or((tail.trim(), false), |tail| (tail.trim(), true))
}

impl Vim {
    /// Execute a direct redirection with the same variable resolution used by scripts.
    pub(super) fn ex_redirect_command(
        &mut self,
        text: &str,
        register: &mut Register,
    ) -> Option<Action> {
        let mut statements = super::script::statements(text);
        let command = parse(statements.next()?)?;
        if statements.next().is_some() {
            return Some(Action::ExPipeline(text.into()));
        }
        let mut program = super::ScriptProgram::user_command(Vec::new(), register.script_source())
            .expect("an empty Ex program is valid");
        let result = program
            .finish_redirection(self, register)
            .and_then(|()| program.start_redirection(self, register, command?));
        Some(match result {
            Ok(action) => action,
            Err(error) => {
                self.script_error(&error);
                Action::None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use std::fs;

    /// Interpret a single Ex command and surface any unexpected failure.
    fn execute(vim: &mut Vim, register: &mut Register, command: &str) -> String {
        assert_eq!(vim.execute_ex_line(command, register), Action::None);
        assert!(!vim.ex_failed(), "{command}: {}", vim.message);
        vim.message.clone()
    }

    #[test]
    fn file_redirection_refuses_overwrite_and_supports_replace_and_append() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("ex.txt");
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        for (command, mode, message, expected) in [
            ("redir > ex.txt", RedirectMode::Create, "alpha", "\nalpha"),
            ("redir >> ex.txt", RedirectMode::Append, "β", "\nalpha\nβ"),
            ("redir! > ex.txt", RedirectMode::Replace, "gamma", "\ngamma"),
        ] {
            assert_eq!(
                vim.execute_ex_line(command, &mut register),
                Action::RedirectFile {
                    path: "ex.txt".into(),
                    mode
                }
            );
            register.input_maps.begin_redirect_file(&path, mode)?;
            assert_eq!(
                execute(&mut vim, &mut register, &format!("echo '{message}'")),
                message
            );
            execute(&mut vim, &mut register, "redi end");
            assert_eq!(fs::read_to_string(&path)?, expected);
        }
        assert_eq!(
            register
                .input_maps
                .begin_redirect_file(&path, RedirectMode::Create)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists,
        );
        assert_eq!(fs::read_to_string(&path)?, "\ngamma");
        Ok(())
    }

    #[test]
    fn register_redirection_is_live_and_appends_with_both_spellings() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        execute(&mut vim, &mut register, "redir @a>");
        execute(&mut vim, &mut register, "echo 'abc'");
        assert_eq!(register.read(Some('a')).text, "\nabc");
        execute(&mut vim, &mut register, "echon 'def'");
        execute(&mut vim, &mut register, "redir @A");
        execute(&mut vim, &mut register, "echo 'ghi'");
        execute(&mut vim, &mut register, "redir @a>>");
        execute(&mut vim, &mut register, "echon 'j'");
        execute(&mut vim, &mut register, "redir END");
        assert_eq!(register.read(Some('a')).text, "\nabcdef\nghij");
        assert!(register.read(None).text.is_empty());
        assert!(
            super::super::script::statements("redir @\" | echo 'x' | redir END").eq([
                "redir @\"",
                "echo 'x'",
                "redir END"
            ])
        );
    }

    #[test]
    fn register_redirection_stays_with_origin_when_another_pane_emits_output() {
        let mut first = Vim::new(Document::new(""));
        let mut other = Vim::new(Document::new(""));
        let mut origin = Register::default();
        let mut second = Register::default();
        second.set_input_mappings(origin.input_maps.clone());

        execute(&mut first, &mut origin, "redir @b");
        execute(&mut other, &mut second, "echo 'from split'");
        assert_eq!(origin.read(Some('b')).text, "\nfrom split");
        assert!(second.read(Some('b')).text.is_empty());
        execute(&mut other, &mut second, "redir END");
        assert_eq!(origin.read(Some('b')).text, "\nfrom split");
    }

    #[test]
    fn variable_redirection_preserves_nested_output_and_reuses_script_scopes() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        execute(
            &mut vim,
            &mut register,
            "function! Capture() | redir => l:value | silent echo 'α' | echon 'β' | echo 'γ' | redir END | let g:result = l:value | endfunction",
        );
        execute(&mut vim, &mut register, "call Capture()");
        assert_eq!(execute(&mut vim, &mut register, "echo g:result"), "\nαβ\nγ");
        execute(&mut vim, &mut register, "redir =>> g:result");
        execute(&mut vim, &mut register, "echo 'δ'");
        execute(&mut vim, &mut register, "redir END");
        assert_eq!(
            execute(&mut vim, &mut register, "echo g:result"),
            "\nαβ\nγ\nδ"
        );

        vim.run_script("let g:dict = {'items': [0, 'old']}\nredir => g:dict.items[-1]\necho 'nested'\nredir END", &mut register).unwrap();
        assert_eq!(
            execute(&mut vim, &mut register, "echo g:dict['items'][1]"),
            "\nnested"
        );

        execute(&mut vim, &mut register, "redir => g:result");
        execute(&mut vim, &mut register, "lockvar g:result");
        assert_eq!(
            vim.execute_ex_line("redir END", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
    }
}
