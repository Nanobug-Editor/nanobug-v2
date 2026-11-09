//! Yank, delete, change and paste operations with document undo checkpoints.

use super::{Register, Vim};
use crate::i18n;

impl Vim {
    /// Apply pending `operator` to `motion`, with combined `count`, updating the shared register.
    pub(super) fn operator(
        &mut self,
        operator: &str,
        motion: &str,
        count: usize,
        register: &mut Register,
    ) {
        let start = self.doc.cursor;
        let row = self.doc.location(start).0;
        let linewise = motion == operator || matches!(motion, "j" | "k");
        let range = if linewise {
            let (first, last) = if motion == "k" {
                (row.saturating_sub(count), row)
            } else {
                (
                    row,
                    (row + count - usize::from(motion == operator)).min(self.doc.starts.len() - 1),
                )
            };
            self.doc.starts[first]
                ..self
                    .doc
                    .starts
                    .get(last + 1)
                    .copied()
                    .unwrap_or(self.doc.text.len())
        } else if motion == "$" {
            start
                ..self
                    .doc
                    .line_range((row + count - 1).min(self.doc.starts.len() - 1))
                    .end
        } else {
            let change_word = operator == "c" && motion == "w" && self.class(start) != 0;
            if !self.motion(if change_word { "e" } else { motion }, count) {
                return;
            }
            let end = self.doc.cursor;
            self.doc.cursor = start;
            let inclusive = motion == "e" || change_word;
            start.min(end)..if inclusive {
                self.doc.next(start.max(end))
            } else {
                start.max(end)
            }
        };
        register.text = self.doc.text[range.clone()].into();
        register.linewise = linewise;
        if linewise && !register.text.ends_with('\n') {
            register.text.push('\n');
        }
        if operator != "y" {
            let at = range.start;
            let changed = !range.is_empty();
            let replacement = if operator == "c" && linewise && range.end < self.doc.text.len() {
                "\n"
            } else {
                ""
            };
            self.doc.replace(range, replacement, true);
            self.doc.cursor = at.min(self.doc.text.len());
            self.clamp_normal();
            if operator == "c" {
                self.begin_insert(changed);
            }
        }
    }

    /// Paste `register` before/after the cursor as chosen by `before`, repeated `count` times.
    pub(super) fn paste(&mut self, register: &Register, before: bool, count: usize) {
        if register.text.is_empty() {
            return;
        }
        let row = self.doc.location(self.doc.cursor).0;
        if register.text.len().saturating_mul(count) > 16 * 1024 * 1024 {
            self.message = i18n::text("editor.paste_too_large").into();
            return;
        }
        let mut text = register.text.repeat(count);
        let at = if register.linewise {
            if before {
                self.doc.starts[row]
            } else if let Some(&next) = self.doc.starts.get(row + 1) {
                next
            } else {
                text = format!("\n{}", text.trim_end_matches('\n'));
                self.doc.text.len()
            }
        } else if before {
            self.doc.cursor
        } else {
            self.doc
                .next(self.doc.cursor)
                .min(self.doc.line_range(row).end)
        };
        self.doc.replace(at..at, &text, true);
        self.doc.cursor = if register.linewise {
            at
        } else {
            self.doc.previous(at + text.len())
        };
        if register.linewise && text.starts_with('\n') {
            self.doc.cursor += 1;
        }
        self.clamp_normal();
    }
}
