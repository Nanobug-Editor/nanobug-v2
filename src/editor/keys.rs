//! Printable Vim command keys are handled before IME translation; text entry keeps native IME.
use gpui::Keystroke;

/// Resolve a printable ASCII command from native `key`, including shifted punctuation.
/// Control/Alt/system combinations and non-character keys are handled by the native shell.
pub fn normal_character(key: &Keystroke) -> Option<String> {
    let modifiers = key.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return None;
    }
    if let Some(text) = &key.key_char
        && text.len() == 1
        && text.is_ascii()
        && !text.as_bytes()[0].is_ascii_control()
    {
        return Some(text.clone());
    }
    if key.key == "space" {
        return Some(" ".into());
    }
    if key.key.len() != 1 || !key.key.is_ascii() {
        return None;
    }
    let mut ch = key.key.as_bytes()[0] as char;
    if modifiers.shift {
        ch = match ch {
            'a'..='z' => ch.to_ascii_uppercase(),
            '1' => '!',
            '2' => '@',
            '3' => '#',
            '4' => '$',
            '5' => '%',
            '6' => '^',
            '7' => '&',
            '8' => '*',
            '9' => '(',
            '0' => ')',
            '-' => '_',
            '=' => '+',
            '[' => '{',
            ']' => '}',
            '\\' => '|',
            ';' => ':',
            '\'' => '"',
            ',' => '<',
            '.' => '>',
            '/' => '?',
            '`' => '~',
            other => other,
        };
    }
    (!ch.is_ascii_control()).then(|| ch.to_string())
}
