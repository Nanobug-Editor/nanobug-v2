//! Ex modifiers that apply to one editor command.

use super::{Action, InputMappings, MessageModifier, Register, Vim};
use crate::document::{MarkModifiers, with_mark_modifiers};
use std::cell::Cell;

/// Canonical modifier names offered by the prompt.
pub(super) const NAMES: &[&str] = &[
    "aboveleft",
    "belowright",
    "botright",
    "horizontal",
    "leftabove",
    "rightbelow",
    "tab",
    "topleft",
    "vertical",
    "browse",
    "confirm",
    "keepalt",
    "keepjumps",
    "keepmarks",
    "keeppatterns",
    "lockmarks",
    "noswapfile",
    "verbose",
];

thread_local! {
    static RETENTION: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

/// Report whether the current Ex operation preserves the window's alternate buffer.
pub fn ex_keeps_alternate() -> bool {
    RETENTION.with(|state| state.get().0)
}

/// Report whether the current Ex operation preserves search and substitution patterns.
pub(super) fn ex_keeps_patterns() -> bool {
    RETENTION.with(|state| state.get().1)
}

/// Restore command retention even when nested command evaluation aborts.
struct RetentionScope((bool, bool));

impl Drop for RetentionScope {
    fn drop(&mut self) {
        RETENTION.with(|state| state.set(self.0));
    }
}

/// Preserve the previous diagnostic level when an Ex command requests a temporary level.
struct VerboseScope<'a>(&'a InputMappings);

impl Drop for VerboseScope<'_> {
    fn drop(&mut self) {
        self.0.end_ex_verbose();
    }
}

/// Refer to a local split edge or an edge of the whole current Tabpage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowPlacement {
    Before,
    After,
    First,
    Last,
}

/// Axis chosen by an explicit Ex modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowOrientation {
    Horizontal,
    Vertical,
}

/// Options carried into the native actions of one Ex command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowModifiers {
    pub placement: Option<WindowPlacement>,
    pub orientation: Option<WindowOrientation>,
    /// Empty text means insert a fresh Tabpage directly after the current one.
    pub tab: Option<String>,
    /// Select an explicit file operand in a native dialog.
    pub browse: bool,
    /// Use confirmation in place of recoverable write or close errors.
    pub confirm: bool,
    /// Disable draft checkpoints for the buffers first loaded by the command.
    pub no_swapfile: bool,
    pub keep_alt: bool,
    pub keep_patterns: bool,
    pub marks: MarkModifiers,
    pub verbose: Option<u32>,
}

impl WindowModifiers {
    /// Keep outer choices unless a nested modifier explicitly overrides them.
    pub fn overlay(&self, inner: Self) -> Self {
        Self {
            placement: inner.placement.or(self.placement),
            orientation: inner.orientation.or(self.orientation),
            tab: inner.tab.or_else(|| self.tab.clone()),
            browse: inner.browse || self.browse,
            confirm: inner.confirm || self.confirm,
            no_swapfile: inner.no_swapfile || self.no_swapfile,
            keep_alt: inner.keep_alt || self.keep_alt,
            keep_patterns: inner.keep_patterns || self.keep_patterns,
            marks: MarkModifiers {
                keep_marks: inner.marks.keep_marks || self.marks.keep_marks,
                keep_jumps: inner.marks.keep_jumps || self.marks.keep_jumps,
                lock_marks: inner.marks.lock_marks || self.marks.lock_marks,
            },
            verbose: inner.verbose.or(self.verbose),
        }
    }

    /// Report whether a command needs window layout, file selection, or prompt handling.
    pub fn has_native_options(&self) -> bool {
        self.placement.is_some()
            || self.orientation.is_some()
            || self.tab.is_some()
            || self.browse
            || self.confirm
            || self.no_swapfile
    }

    /// Apply only the state owned by the command currently evaluated.
    pub fn with_ex_state<T>(&self, mappings: &InputMappings, run: impl FnOnce() -> T) -> T {
        let _verbose = self.verbose.map(|level| {
            mappings.begin_ex_verbose(level);
            VerboseScope(mappings)
        });
        let _retention = RetentionScope(RETENTION.with(|state| {
            let (keep_alt, keep_patterns) = state.get();
            state.replace((
                self.keep_alt || keep_alt,
                self.keep_patterns || keep_patterns,
            ))
        }));
        let marks = crate::document::current_mark_modifiers();
        with_mark_modifiers(
            MarkModifiers {
                keep_marks: self.marks.keep_marks || marks.keep_marks,
                keep_jumps: self.marks.keep_jumps || marks.keep_jumps,
                lock_marks: self.marks.lock_marks || marks.lock_marks,
            },
            run,
        )
    }
}

/// Clear keepjumps in commands indirectly invoked through scripts or functions.
pub fn without_ex_keep_jumps<T>(run: impl FnOnce() -> T) -> T {
    let mut marks = crate::document::current_mark_modifiers();
    marks.keep_jumps = false;
    with_mark_modifiers(marks, run)
}

impl Vim {
    /// Consume a chain of window modifiers, retaining its untouched inner command and addresses.
    pub(super) fn ex_window_modifiers(&mut self, text: &str) -> Option<Action> {
        let (range, command) = text
            .strip_prefix("'<,'>")
            .map_or(("", text), |rest| ("'<,'>", rest));
        let (modifiers, tail) = parse_modifiers(command)?;
        if tail.is_empty() {
            self.ex_failed = true;
            self.message =
                crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
            return Some(Action::None);
        }
        Some(Action::WindowModified {
            modifiers,
            command: format!("{range}{tail}"),
        })
    }

    /// Expose the command itself to a source frame that may need to wait for native work.
    pub fn source_ex_modifiers<'a>(
        &self,
        text: &'a str,
        register: &Register,
    ) -> Option<(WindowModifiers, Vec<MessageModifier>, &'a str)> {
        if self.collecting_ex_lines() || super::script::defining_function(register) {
            return None;
        }
        let (modifiers, messages, tail) = command_modifiers(text);
        if tail.is_empty()
            || modifiers.has_native_options()
            || (modifiers == WindowModifiers::default() && messages.is_empty())
            || (!messages.is_empty()
                && !self
                    .ex_range(tail)
                    .is_some_and(|(_, body)| body.starts_with('!')))
        {
            return None;
        }
        Some((modifiers, messages, tail))
    }
}

/// Separate command and message prefixes from the text they scope.
pub(super) fn command_modifiers(text: &str) -> (WindowModifiers, Vec<MessageModifier>, &str) {
    let mut tail = text.trim_start_matches([' ', '\t', ':']);
    let mut modifiers = WindowModifiers::default();
    let mut messages = Vec::new();
    loop {
        if let Some((parsed, rest)) = parse_modifiers(tail) {
            modifiers = modifiers.overlay(parsed);
            tail = rest;
        } else if let Some(Ok((parsed, rest))) = super::ex_message_modifiers::parse_modifier(tail) {
            messages.push(parsed);
            tail = rest.trim_start_matches([' ', '\t', ':']);
        } else {
            return (modifiers, messages, tail);
        }
    }
}

/// Split a modifier chain from its untouched inner command and address.
pub(super) fn parse_modifiers(text: &str) -> Option<(WindowModifiers, &str)> {
    let mut tail = text.trim_start();
    let mut modifiers = WindowModifiers::default();
    while let Some((part, rest)) = parse_modifier(tail) {
        modifiers = modifiers.overlay(part);
        tail = rest.trim_start_matches([' ', '\t', ':']);
    }
    (modifiers != WindowModifiers::default()).then_some((modifiers, tail))
}

/// Split exactly one modifier and its optional Tabpage address or verbosity level.
fn parse_modifier(text: &str) -> Option<(WindowModifiers, &str)> {
    let prefix = text
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || b".$+-".contains(byte))
        .count();
    let address = &text[..prefix];
    let body = text[prefix..].trim_start();
    let end = body.bytes().take_while(u8::is_ascii_alphabetic).count();
    let (name, tail) = body.split_at(end);
    if !tail.is_empty() && !tail.starts_with(char::is_whitespace) && !tail.starts_with(':') {
        return None;
    }
    let name_is = |full: &str, minimum| name.len() >= minimum && full.starts_with(name);
    let mut modifiers = WindowModifiers::default();
    if name == "tab" {
        modifiers.tab = Some(address.to_owned());
    } else if name_is("verbose", 4) {
        modifiers.verbose = Some(if address.is_empty() {
            1
        } else {
            address.parse().ok()?
        });
    } else if !address.is_empty() {
        return None;
    } else if name_is("browse", 3) {
        modifiers.browse = true;
    } else if name_is("confirm", 4) {
        modifiers.confirm = true;
    } else if name_is("keepalt", 5) {
        modifiers.keep_alt = true;
    } else if name_is("keepjumps", 5) {
        modifiers.marks.keep_jumps = true;
    } else if name_is("keepmarks", 3) {
        modifiers.marks.keep_marks = true;
    } else if name_is("keeppatterns", 5) {
        modifiers.keep_patterns = true;
    } else if name_is("lockmarks", 3) {
        modifiers.marks.lock_marks = true;
    } else if name_is("noswapfile", 3) {
        modifiers.no_swapfile = true;
    } else if name_is("aboveleft", 3) || name_is("leftabove", 5) {
        modifiers.placement = Some(WindowPlacement::Before);
    } else if name_is("belowright", 3) || name_is("rightbelow", 6) {
        modifiers.placement = Some(WindowPlacement::After);
    } else if name_is("topleft", 2) {
        modifiers.placement = Some(WindowPlacement::First);
    } else if name_is("botright", 2) {
        modifiers.placement = Some(WindowPlacement::Last);
    } else if name_is("vertical", 4) {
        modifiers.orientation = Some(WindowOrientation::Vertical);
    } else if name_is("horizontal", 3) {
        modifiers.orientation = Some(WindowOrientation::Horizontal);
    } else {
        return None;
    }
    Some((modifiers, tail))
}

/// Return a modifier's command line suffix after the user types a separating character.
pub(super) fn completion_tail(text: &str) -> Option<&str> {
    let (_, rest) = parse_modifier(text)?;
    (!rest.is_empty()).then(|| rest.trim_start_matches([' ', '\t', ':']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Compose direction, placement and an independent window address on the nested command.
    #[test]
    fn window_modifiers_chain_with_tab_address_and_inner_count() {
        let mut vim = Vim::new(Document::new("x\n"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line("vertical leftabove +2tab 3sbuffer", &mut register),
            Action::WindowModified {
                modifiers: WindowModifiers {
                    placement: Some(WindowPlacement::Before),
                    orientation: Some(WindowOrientation::Vertical),
                    tab: Some("+2".into()),
                    ..WindowModifiers::default()
                },
                command: "3sbuffer".into(),
            }
        );
        assert_eq!(
            vim.execute_ex_line("rightbelow horizontal topleft 5resize", &mut register),
            Action::WindowModified {
                modifiers: WindowModifiers {
                    placement: Some(WindowPlacement::First),
                    orientation: Some(WindowOrientation::Horizontal),
                    ..WindowModifiers::default()
                },
                command: "5resize".into(),
            }
        );
    }

    /// The abbreviated swap modifier composes with a split and accepts a nested Ex command.
    #[test]
    fn noswapfile_overlays_other_ex_modifiers() {
        let mut vim = Vim::new(Document::new("x\n"));
        let mut register = Register::default();
        assert_eq!(
            vim.execute_ex_line(r"nos vertical split a\ b.rs", &mut register),
            Action::WindowModified {
                modifiers: WindowModifiers {
                    orientation: Some(WindowOrientation::Vertical),
                    no_swapfile: true,
                    ..WindowModifiers::default()
                },
                command: r"split a\ b.rs".into(),
            },
        );
        assert_eq!(
            vim.execute_ex_line("noswapfile", &mut register),
            Action::None
        );
        assert!(vim.ex_failed());
    }

    /// Restore both patterns after a substitution or motion and restore verbose after the command.
    #[test]
    fn ex_state_retains_patterns_and_scopes_verbosity() {
        let mut vim = Vim::new(Document::new("before\ntarget\nbefore\nword word\n"));
        let mut register = Register::default();
        vim.execute_ex_line("1s/before/saved/", &mut register);
        assert_eq!(vim.search_pattern(), Some("before"));
        vim.execute_ex_line("keepp 2s/target/changed/", &mut register);
        assert!(!vim.ex_failed(), "{}", vim.message);
        assert_eq!(vim.search_pattern(), Some("before"));
        vim.execute_ex_line("3&", &mut register);
        assert_eq!(vim.doc.text, "saved\nchanged\nsaved\nword word\n");
        vim.set_cursor(vim.doc.starts[3]);
        vim.execute_ex_line("keeppatterns normal! *", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (3, 5));
        assert_eq!(vim.search_pattern(), Some("before"));

        vim.execute_ex_line("4verbose 0verb set verbose?", &mut register);
        assert_eq!(vim.message, "verbose=0");
        vim.execute_ex_line("verb set verbose?", &mut register);
        assert_eq!(vim.message, "verbose=1");
        assert_eq!(register.input_maps.ex_verbose(), 0);
    }

    /// Keep shell pipelines and their scope together through script lexing.
    #[test]
    fn source_preserves_scoped_shell_pipeline() {
        let vim = Vim::new(Document::new("first\nsecond\n"));
        let register = Register::default();
        let statement = "silent kee 1,2! producer | consumer";
        assert_eq!(vim.source_statements(statement), vec![statement]);
        let (modifiers, messages, command) = vim.source_ex_modifiers(statement, &register).unwrap();
        assert!(modifiers.marks.keep_marks);
        assert!(matches!(
            messages.as_slice(),
            [MessageModifier::Silent {
                ignore_error: false
            }]
        ));
        assert_eq!(command, "1,2! producer | consumer");
        let lines = vim.script_lines("keepjumps append\n  literal   | text\n.\n", 1);
        assert!(lines[1].literal);
        assert_eq!(lines[1].text, "  literal   | text");
    }
}
