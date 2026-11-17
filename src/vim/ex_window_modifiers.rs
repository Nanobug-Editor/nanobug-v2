//! Ex modifiers that apply to a command's native editor window operations.

use super::{Action, Vim};

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
];

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
        }
    }
}

impl Vim {
    /// Consume a chain of window modifiers, retaining its untouched inner command and addresses.
    pub(super) fn ex_window_modifiers(&mut self, text: &str) -> Option<Action> {
        let mut tail = text.trim_start();
        let mut modifiers = WindowModifiers::default();
        while let Some((part, rest)) = parse_modifier(tail) {
            modifiers = modifiers.overlay(part);
            tail = rest.trim_start_matches([' ', '\t', ':']);
        }
        if modifiers == WindowModifiers::default() {
            return None;
        }
        if tail.is_empty() {
            self.ex_failed = true;
            self.message =
                crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
            return Some(Action::None);
        }
        Some(Action::WindowModified {
            modifiers,
            command: tail.to_owned(),
        })
    }
}

/// Split exactly one modifier and any Tabpage address preceding it.
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
    } else if !address.is_empty() {
        return None;
    } else if name_is("browse", 3) {
        modifiers.browse = true;
    } else if name_is("confirm", 4) {
        modifiers.confirm = true;
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
}
