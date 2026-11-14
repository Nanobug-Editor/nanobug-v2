//! Add and subtract numbers in Normal mode with the default binary and hexadecimal prefixes.

use super::Vim;
use regex::Regex;
use std::sync::LazyLock;

/// Number forms understood by the editor's default nrformats.
static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"0[xX][0-9a-fA-F]+|0[bB][01]+|-?[0-9]+").expect("number pattern"));

impl Vim {
    /// Add or subtract `count` from the number at or after the caret.
    pub(super) fn adjust_number(&mut self, increase: bool, count: usize) {
        let row = self.doc.location(self.doc.cursor).0;
        let start = self.doc.starts[row];
        let line = self.doc.line(row);
        let cursor = self.doc.cursor - start;
        let Some(number) = NUMBER.find_iter(line).find(|number| number.end() > cursor) else {
            return;
        };
        let original = number.as_str();
        let (prefix, digits, radix) = if original.starts_with("0x") || original.starts_with("0X") {
            (&original[..2], &original[2..], 16)
        } else if original.starts_with("0b") || original.starts_with("0B") {
            (&original[..2], &original[2..], 2)
        } else {
            ("", original.trim_start_matches('-'), 10)
        };
        let Ok(value) = u64::from_str_radix(digits, radix) else {
            return;
        };
        let delta = count as u64;
        let width = if digits.starts_with('0') {
            digits.len()
        } else {
            0
        };
        let text = if radix == 10 {
            let value = if original.starts_with('-') {
                -(value as i128)
            } else {
                value as i128
            };
            let value = value
                + if increase {
                    delta as i128
                } else {
                    -(delta as i128)
                };
            let digits = format!("{:0width$}", value.unsigned_abs());
            if value < 0 {
                format!("-{digits}")
            } else {
                digits
            }
        } else {
            let value = if increase {
                value.wrapping_add(delta)
            } else {
                value.wrapping_sub(delta)
            };
            let digits = match radix {
                2 => format!("{value:0width$b}"),
                _ if original[2..].chars().any(|ch| matches!(ch, 'A'..='F')) => {
                    format!("{value:0width$X}")
                }
                _ => format!("{value:0width$x}"),
            };
            format!("{prefix}{digits}")
        };
        let begin = start + number.start();
        self.replace(begin..start + number.end(), &text, true);
        self.set_cursor(self.doc.previous(begin + text.len()));
    }
}
