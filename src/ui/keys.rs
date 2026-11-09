//! Printable Vim command keys are handled before IME translation; text entry keeps native IME.
use gpui::Keystroke;

/// Resolve a printable ASCII command from native `key`, including shifted punctuation.
/// Letter commands use the key and held Shift because native text also reflects Caps Lock.
/// Control/Alt/system combinations and non-character keys are handled by the native shell.
pub fn normal_character(key: &Keystroke) -> Option<String> {
    let modifiers = key.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return None;
    }
    if let [letter] = key.key.as_bytes()
        && letter.is_ascii_alphabetic()
    {
        let letter = char::from(*letter);
        let command = if modifiers.shift {
            letter.to_ascii_uppercase()
        } else {
            letter.to_ascii_lowercase()
        };
        return Some(command.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    #[test]
    fn normal_keys_work_without_ime_commits_and_preserve_shifted_commands() {
        for (raw, shift, expected) in [
            ("i", false, "i"),
            ("g", true, "G"),
            (";", true, ":"),
            ("4", true, "$"),
            ("6", true, "^"),
        ] {
            let key = Keystroke {
                key: raw.into(),
                key_char: None,
                modifiers: Modifiers {
                    shift,
                    ..Default::default()
                },
            };
            assert_eq!(normal_character(&key).as_deref(), Some(expected));
        }
        let control = Keystroke {
            key: "r".into(),
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(normal_character(&control), None);
    }
}
