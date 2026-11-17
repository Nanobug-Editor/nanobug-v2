//! Literal key and Unicode digraph input for editor text and command prompts.

use super::{Action, Operator, Pending, Register, Vim};
use unicode_segmentation::UnicodeSegmentation;

/// A pending argument to Ctrl+v, Ctrl+q or Ctrl+k.
#[derive(Clone, Copy)]
pub(super) enum Argument {
    /// Read one literal key or start a number with a base selector.
    Literal {
        /// Shortcut label, also determining whether control keys become control characters.
        prefix: &'static str,
        /// Command consuming the decoded character.
        target: Target,
    },
    /// Read a numeric codepoint.
    Number {
        /// Shortcut that introduced this codepoint.
        prefix: &'static str,
        /// Command consuming the decoded codepoint.
        target: Target,
        /// Decimal, octal or hexadecimal base.
        radix: u32,
        /// Available digit positions in this numeric form.
        remaining: u8,
        /// Codepoint accumulated from any digits already entered.
        value: Option<u32>,
    },
    /// Read the first or second character of a digraph.
    Digraph {
        /// First character, if entered.
        first: Option<char>,
        /// Command consuming the resulting digraph.
        target: Target,
    },
}

/// Consumer of the decoded character or literal special key.
#[derive(Clone, Copy)]
pub(super) enum Target {
    /// Enter text in Insert, Replace or a command prompt.
    Text,
    /// A normal or virtual replacement with its character count.
    Replace(usize, bool),
    /// A character-find motion with direction, count, and optional pending operator.
    Find(char, usize, Option<Operator>),
}

/// Nanobug digraphs for accented Latin letters and Greek transliteration.
/// Each row pairs an ASCII base letter with its precomposed Unicode character at the same index.
const DIACRITICS: &[(char, &str, &str)] = &[
    (
        '\'',
        "AaCcEeGgIiKkLlMmNnOoPpRrSsUuWwYyZz",
        "ÁáĆćÉéǴǵÍíḰḱĹĺḾḿŃńÓóṔṕŔŕŚśÚúẂẃÝýŹź",
    ),
    ('`', "AaEeIiNnOoUuWwYy", "ÀàÈèÌìǸǹÒòÙùẀẁỲỳ"),
    (
        '^',
        "AaCcEeGgHhIiJjOoSsUuWwYyZz",
        "ÂâĈĉÊêĜĝĤĥÎîĴĵÔôŜŝÛûŴŵŶŷẐẑ",
    ),
    (':', "AaEeHhIiOoUuWwXxYy", "ÄäËëḦḧÏïÖöÜüẄẅẌẍŸÿ"),
    ('~', "AaEeIiNnOoUuVvYy", "ÃãẼẽĨĩÑñÕõŨũṼṽỸỹ"),
    (',', "CcDdEeGgHhKkLlNnRrSsTt", "ÇçḐḑȨȩĢģḨḩĶķĻļŅņŖŗŞşŢţ"),
    ('-', "AaEeGgIiOoUuYy", "ĀāĒēḠḡĪīŌōŪūȲȳ"),
    (
        '.',
        "AaBbCcDdEeFfGgHhIMmNnOoPpRrSsTtWwXxYyZz",
        "ȦȧḂḃĊċḊḋĖėḞḟĠġḢḣİṀṁṄṅȮȯṖṗṘṙṠṡṪṫẆẇẊẋẎẏŻż",
    ),
    (
        'v',
        "AaCcDdEeGgHhIiKkLlNnOoRrSsTtUuZz",
        "ǍǎČčĎďĚěǦǧȞȟǏǐǨǩĽľŇňǑǒŘřŠšŤťǓǔŽž",
    ),
    ('(', "AaEeGgIiOoUu", "ĂăĔĕĞğĬĭŎŏŬŭ"),
    (';', "AaEeIiOoUu", "ĄąĘęĮįǪǫŲų"),
    ('"', "OoUu", "ŐőŰű"),
    ('*', "ABGDEZHQIKLMNXOPRSTUFCYW", "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩ"),
    ('*', "abgdezhqiklmnxoprstufcyw", "αβγδεζηθικλμνξοπρστυφχψω"),
];

/// Project-maintained pairs for frequently typed symbols, fractions, and letters without accents.
const DIGRAPHS: &[([char; 2], char)] = &[
    (['A', 'A'], 'Å'),
    (['a', 'a'], 'å'),
    (['A', 'E'], 'Æ'),
    (['a', 'e'], 'æ'),
    (['O', 'E'], 'Œ'),
    (['o', 'e'], 'œ'),
    (['D', '-'], 'Ð'),
    (['d', '-'], 'ð'),
    (['T', 'H'], 'Þ'),
    (['t', 'h'], 'þ'),
    (['L', '/'], 'Ł'),
    (['l', '/'], 'ł'),
    (['O', '/'], 'Ø'),
    (['o', '/'], 'ø'),
    (['s', 's'], 'ß'),
    (['<', '<'], '«'),
    (['>', '>'], '»'),
    (['<', '1'], '‹'),
    (['>', '1'], '›'),
    (['!', '!'], '¡'),
    (['?', '?'], '¿'),
    (['-', '-'], '–'),
    (['-', 'M'], '—'),
    (['.', '.'], '…'),
    (['.', '*'], '•'),
    (['C', 'o'], '©'),
    (['R', 'g'], '®'),
    (['T', 'M'], '™'),
    (['E', 'u'], '€'),
    (['L', 'b'], '£'),
    (['Y', 'n'], '¥'),
    (['c', '$'], '¢'),
    (['/', '/'], '÷'),
    (['x', 'x'], '×'),
    (['+', '-'], '±'),
    (['-', '+'], '∓'),
    (['0', '^'], '°'),
    (['m', 'u'], 'µ'),
    (['<', '='], '≤'),
    (['>', '='], '≥'),
    (['!', '='], '≠'),
    (['~', '='], '≈'),
    (['=', '='], '≡'),
    (['o', 'o'], '∞'),
    (['s', 'q'], '√'),
    (['S', 'm'], '∑'),
    (['I', 'n'], '∫'),
    (['<', '-'], '←'),
    (['-', '>'], '→'),
    (['^', '^'], '↑'),
    (['v', 'v'], '↓'),
    (['1', '2'], '½'),
    (['1', '3'], '⅓'),
    (['2', '3'], '⅔'),
    (['1', '4'], '¼'),
    (['3', '4'], '¾'),
    (['1', '5'], '⅕'),
    (['2', '5'], '⅖'),
    (['3', '5'], '⅗'),
    (['4', '5'], '⅘'),
    (['1', '6'], '⅙'),
    (['5', '6'], '⅚'),
    (['1', '7'], '⅐'),
    (['1', '8'], '⅛'),
    (['3', '8'], '⅜'),
    (['5', '8'], '⅝'),
    (['7', '8'], '⅞'),
    (['1', '9'], '⅑'),
    (['1', '0'], '⅒'),
];

impl Vim {
    /// Start a literal or digraph argument after `key`.
    pub(super) fn begin_literal_input(&mut self, key: &str) {
        self.begin_character_argument(key, Target::Text);
    }

    /// Start decoding an argument after literal or digraph shortcut `key` for `target`.
    pub(super) fn begin_character_argument(&mut self, key: &str, target: Target) {
        let prefix = match key {
            "Ctrl+k" => {
                self.pending = Some(Pending::InsertArgument(Argument::Digraph {
                    first: None,
                    target,
                }));
                return;
            }
            "Ctrl+q" => "Ctrl+q",
            "Ctrl+Shift+v" => "Ctrl+Shift+v",
            "Ctrl+Shift+q" => "Ctrl+Shift+q",
            _ => "Ctrl+v",
        };
        let argument = Argument::Literal { prefix, target };
        self.pending = Some(Pending::InsertArgument(argument));
    }

    /// Consume `key` for a literal `argument`, forwarding a key after a short number through `register`.
    pub(super) fn insert_argument(
        &mut self,
        argument: Argument,
        key: &str,
        register: &mut Register,
    ) -> Action {
        match argument {
            Argument::Literal { prefix, target } => {
                let number = match key {
                    "o" | "O" => Some((8, 3, None)),
                    "x" | "X" => Some((16, 2, None)),
                    "u" => Some((16, 4, None)),
                    "U" => Some((16, 8, None)),
                    _ => single_character(key)
                        .and_then(|ch| ch.to_digit(10))
                        .map(|digit| (10, 2, Some(digit))),
                };
                if let Some((radix, remaining, value)) = number {
                    self.pending = Some(Pending::InsertArgument(Argument::Number {
                        prefix,
                        target,
                        radix,
                        remaining,
                        value,
                    }));
                } else if let Some(ch) = literal_character(key)
                    && !(prefix.contains("Shift") && key.starts_with("Ctrl+"))
                {
                    self.apply_character_argument(target, ch.encode_utf8(&mut [0; 4]), register);
                } else if key.graphemes(true).count() == 1 {
                    self.apply_character_argument(target, key, register);
                } else {
                    self.apply_character_argument(target, &key_notation(key), register);
                }
            }
            Argument::Number {
                prefix,
                target,
                radix,
                remaining,
                value,
            } => {
                if let Some(digit) = single_character(key).and_then(|ch| ch.to_digit(radix)) {
                    let value = value.unwrap_or(0) * radix + digit;
                    if remaining == 1 {
                        self.insert_codepoint(radix, value, target, register);
                    } else {
                        self.pending = Some(Pending::InsertArgument(Argument::Number {
                            prefix,
                            target,
                            radix,
                            remaining: remaining - 1,
                            value: Some(value),
                        }));
                    }
                } else {
                    if let Some(value) = value {
                        self.insert_codepoint(radix, value, target, register);
                    }
                    return self.handle_key(key, register, &mut |_, _, _, _| None);
                }
            }
            Argument::Digraph { first, target } => {
                if matches!(key, "Escape" | "Ctrl+c" | "Ctrl+[") {
                    return Action::None;
                }
                if let Some(ch) = literal_character(key) {
                    if let Some(first) = first {
                        let combined = lookup_digraph(first, ch, register).unwrap_or(ch);
                        self.apply_character_argument(
                            target,
                            combined.encode_utf8(&mut [0; 4]),
                            register,
                        );
                    } else {
                        self.pending = Some(Pending::InsertArgument(Argument::Digraph {
                            first: Some(ch),
                            target,
                        }));
                    }
                } else {
                    self.apply_character_argument(target, &key_notation(key), register);
                }
            }
        }
        Action::None
    }

    /// Decode numeric `value` in `radix` and pass it to `target` with `register`.
    fn insert_codepoint(
        &mut self,
        radix: u32,
        value: u32,
        target: Target,
        register: &mut Register,
    ) {
        let value = if radix == 16 { value } else { value.min(255) };
        if let Some(ch) = char::from_u32(value) {
            self.apply_character_argument(target, ch.encode_utf8(&mut [0; 4]), register);
        }
    }

    /// Pass the decoded `text` to `target`, applying an optional operator through `register`.
    fn apply_character_argument(&mut self, target: Target, text: &str, register: &mut Register) {
        match target {
            Target::Text if self.mode.is_insert() => self.insert_text(text),
            Target::Text => self.command_insert(text),
            Target::Replace(count, virtual_replace) => {
                let text = if text == "\n" { "\0" } else { text };
                if virtual_replace && text != "\r" {
                    self.replace_virtual_characters(text, count, register);
                } else {
                    self.replace_chars(text, count);
                }
            }
            Target::Find(key, count, operator) => {
                if let Some(motion) = self.find_motion(key, text, count) {
                    if let Some(operator) = operator {
                        self.operate_motion(operator, motion, register);
                    } else {
                        self.apply_motion(motion);
                    }
                }
                self.selected_register = None;
            }
        }
    }
}

impl Target {
    /// Visible prefix of the command awaiting literal input.
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::Text => "",
            Self::Replace(_, false) => "r ",
            Self::Replace(_, true) => "gr ",
            Self::Find('f', _, _) => "f ",
            Self::Find('F', _, _) => "F ",
            Self::Find('t', _, _) => "t ",
            Self::Find(_, _, _) => "T ",
        }
    }
}

/// Spell a semantic `key` as Vim notation when it has no literal character.
fn key_notation(key: &str) -> String {
    let key = key.replace("Ctrl+", "C-").replace("Shift+", "S-");
    format!("<{key}>")
}

/// Resolve `key` only when it already names a single Unicode scalar.
fn single_character(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

/// Convert a symbolic `key` to its literal character when it has one.
fn literal_character(key: &str) -> Option<char> {
    match key {
        "Escape" => Some('\u{1b}'),
        "Enter" => Some('\r'),
        "Tab" => Some('\t'),
        "Backspace" => Some('\u{8}'),
        "Delete" => Some('\u{7f}'),
        "Ctrl+space" | "Ctrl+@" => Some('\0'),
        _ => single_character(key).or_else(|| {
            let ch = single_character(key.strip_prefix("Ctrl+")?)?.to_ascii_uppercase();
            match ch {
                '@'..='_' => char::from_u32(u32::from(ch) - u32::from('@')),
                '?' => Some('\u{7f}'),
                _ => None,
            }
        }),
    }
}

/// Look up two digraph characters, also trying the reverse order.
fn digraph(first: char, second: char) -> Option<char> {
    if first == ' ' && second.is_ascii() {
        return char::from_u32(u32::from(second) | 0x80);
    }
    if !first.is_ascii() || !second.is_ascii() {
        return None;
    }
    [(first, second), (second, first)]
        .into_iter()
        .find_map(|(a, b)| {
            DIGRAPHS
                .iter()
                .find_map(|(keys, value)| (*keys == [a, b]).then_some(*value))
                .or_else(|| {
                    DIACRITICS
                        .iter()
                        .filter(|(accent, ..)| *accent == b)
                        .find_map(|(_, letters, values)| {
                            letters
                                .chars()
                                .zip(values.chars())
                                .find_map(|(letter, value)| (letter == a).then_some(value))
                        })
                })
        })
}

/// Enumerate the project's maintained default digraphs in lookup order.
pub(super) fn default_digraphs() -> impl Iterator<Item = ([char; 2], char)> {
    DIGRAPHS
        .iter()
        .copied()
        .chain(DIACRITICS.iter().flat_map(|&(accent, letters, values)| {
            letters
                .chars()
                .zip(values.chars())
                .map(move |(letter, value)| ([letter, accent], value))
        }))
}

/// Resolve custom and built-in digraphs with each pair's original order first.
pub(super) fn lookup_digraph(first: char, second: char, register: &Register) -> Option<char> {
    register.input_maps.digraphs(|table| {
        table
            .get(&[first, second])
            .copied()
            .or_else(|| {
                default_digraphs()
                    .find_map(|(key, value)| (key == [first, second]).then_some(value))
            })
            .or_else(|| table.get(&[second, first]).copied())
            .or_else(|| digraph(first, second))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use std::collections::BTreeSet;

    /// Check that each maintained key pair refers to a single Unicode character.
    #[test]
    fn own_digraphs_have_aligned_and_unique_pairs() {
        let mut keys = BTreeSet::new();
        for (accent, letters, values) in DIACRITICS {
            assert_eq!(letters.chars().count(), values.chars().count(), "{accent}");
            for letter in letters.chars() {
                assert!(keys.insert([letter, *accent]), "{letter}{accent}");
            }
        }
        for (pair, _) in DIGRAPHS {
            assert!(keys.insert(*pair), "{pair:?}");
        }
    }

    /// Compose Latin accents, fractions and symbols through the editor's Ctrl+k input.
    #[test]
    fn ctrl_k_inserts_own_digraphs_and_keeps_pair_order() {
        let mut editor = Vim::new(Document::new(""));
        let mut register = Register::default();
        for key in [
            "i", "Ctrl+k", "e", "'", "Ctrl+k", ":", "a", "Ctrl+k", "1", "2", "Ctrl+k", "<", "<",
            "Ctrl+k", "+", "-", "Ctrl+k", "-", "+", "Ctrl+k", "9", "q", "Escape",
        ] {
            assert_eq!(editor.key(key, &mut register), Action::None);
        }
        assert_eq!(editor.doc.text, "éä½«±∓q");
    }
}
