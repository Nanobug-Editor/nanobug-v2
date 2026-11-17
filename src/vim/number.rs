//! Add and subtract selected numbers with the default binary and hexadecimal prefixes.

use super::{Mode, Vim};
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
        let begin = start + number.start();
        let Some(text) = replacement(number.as_str(), increase, count) else {
            return;
        };
        self.replace(begin..start + number.end(), &text, true);
        self.set_cursor(self.doc.previous(begin + text.len()));
    }

    /// Adjust the first selected number per row by `count`, accumulating the step on
    /// rows with a number when `progressive` is set. `increase` chooses the sign.
    pub(super) fn adjust_visual_numbers(
        &mut self,
        increase: bool,
        count: usize,
        progressive: bool,
    ) {
        let first = self.doc.location(self.anchor.min(self.doc.cursor)).0;
        let last = self.doc.location(self.anchor.max(self.doc.cursor)).0;
        let first_byte = self
            .selection_on_line(first)
            .map_or(self.doc.starts[first], |selected| selected.start);
        let mut edits = Vec::new();
        for row in first..=last {
            let selected = self.selection_on_line(row).expect("selected row");
            let line = self.doc.line_range(row);
            let start = selected.start.max(line.start);
            let end = selected.end.min(line.end);
            let Some(number) = NUMBER.find(&self.doc.text[start..end]) else {
                continue;
            };
            let step = if progressive {
                count.saturating_mul(edits.len() + 1)
            } else {
                count
            };
            if let Some(text) = replacement(number.as_str(), increase, step) {
                edits.push((start + number.start()..start + number.end(), text));
            }
        }
        for (idx, (range, text)) in edits.into_iter().rev().enumerate() {
            self.replace(range, &text, idx == 0);
        }
        self.mode = Mode::Normal;
        self.set_cursor(first_byte);
    }
}

/// Format `original` plus or minus `count`, preserving its radix, case and zero padding.
fn replacement(original: &str, increase: bool, count: usize) -> Option<String> {
    let (prefix, digits, radix) = if original.starts_with("0x") || original.starts_with("0X") {
        (&original[..2], &original[2..], 16)
    } else if original.starts_with("0b") || original.starts_with("0B") {
        (&original[..2], &original[2..], 2)
    } else {
        ("", original.trim_start_matches('-'), 10)
    };
    let value = u64::from_str_radix(digits, radix).ok()?;
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
    Some(text)
}
