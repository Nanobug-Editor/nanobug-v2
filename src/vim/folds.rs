//! Translate modal folding keys into retained document folds and motions.

use super::{
    Mode, Operator, Pending, Vim,
    motions::{Kind, Motion},
};
use crate::{
    document::{FoldChange, FoldEdge, FoldLevel},
    i18n,
};
use std::ops::Range;

/// Internal operator identity for `zf` and its resolved line ranges.
pub(super) const FOLD_OPERATOR: char = '\0';

impl Vim {
    /// Open or close one fold level in `rows`, recursing when the Ex command uses a bang.
    pub(in crate::vim) fn ex_change_fold_rows(
        &mut self,
        rows: Range<usize>,
        close: bool,
        recursive: bool,
    ) {
        if self.doc.change_ex_fold_rows(rows, close, recursive) {
            self.move_out_of_hidden_fold();
        } else {
            self.message = i18n::text("editor.no_fold").into();
        }
    }

    /// Open one collapsed depth at document `row` while retaining the cursor's source offset.
    pub fn open_fold_at_header(&mut self, row: usize) -> bool {
        if !self.doc.open_fold_at_header(row) {
            return false;
        }
        if !self.fold_cursor_on_closing_brace() {
            self.folded_closing_caret = None;
        }
        true
    }

    /// Whether the Normal-mode caret is on the synthetic closing brace of a collapsed row.
    pub fn fold_cursor_on_closing_brace(&self) -> bool {
        self.mode == Mode::Normal
            && self.folded_closing_caret == Some(self.doc.cursor)
            && self
                .doc
                .folded_braces(self.doc.location(self.doc.cursor).0)
                .is_some_and(|(open, _)| open == self.doc.cursor)
    }

    /// Match the visible brace pair for Normal-mode `%` within a folded preview.
    pub(super) fn move_between_fold_braces(&mut self) -> bool {
        if self.mode != Mode::Normal {
            return false;
        }
        let row = self.doc.location(self.doc.cursor).0;
        let Some((open, _)) = self.doc.folded_braces(row) else {
            return false;
        };
        if self.doc.cursor != open {
            return false;
        }
        self.folded_closing_caret = (!self.fold_cursor_on_closing_brace()).then_some(open);
        self.desired_column = None;
        true
    }

    /// Move horizontally across the collapsed brace pair, treating the preview as one step.
    /// Return whether the collapsed preview handled `key` with its requested `count`.
    pub(super) fn move_on_fold_preview(&mut self, key: &str, count: usize) -> bool {
        if self.mode != Mode::Normal
            || !matches!(
                key,
                "h" | "Left" | "Backspace" | "Ctrl+h" | "l" | "Right" | " " | "$" | "End"
            )
        {
            return false;
        }
        let right = matches!(key, "l" | "Right" | " ");
        let row = self.doc.location(self.doc.cursor).0;
        let Some((open, _)) = self.doc.folded_braces(row) else {
            return false;
        };
        if matches!(key, "$" | "End") {
            if count > 1 {
                return false;
            }
            self.doc.cursor = open;
            self.folded_closing_caret = Some(open);
            self.desired_column = Some(usize::MAX);
            return true;
        }
        if self.fold_cursor_on_closing_brace() {
            if !right {
                self.folded_closing_caret = None;
                if count > 1
                    && let Some(motion) = self.cursor_motion(key, Some(count - 1))
                {
                    self.apply_motion(motion);
                }
            }
        } else if right {
            if self.doc.cursor > open {
                return false;
            }
            let Some(motion) = self.cursor_motion(key, Some(count)) else {
                return false;
            };
            if motion.end <= open {
                return false;
            }
            self.doc.cursor = open;
            self.folded_closing_caret = Some(open);
        } else {
            return false;
        }
        self.desired_column = None;
        true
    }

    /// Keep the cursor on a visible row after an input stroke, restoring any search folds it left.
    /// Forward movement skips the body; backward movement lands on its visible closing brace or header.
    pub(super) fn settle_fold_cursor(&mut self, previous: usize, revision: u64) {
        let row = self.doc.location(self.doc.cursor).0;
        let edited = revision != self.doc.revision;
        if edited {
            self.doc.reveal_row(row);
        } else if let Some(body) = self.doc.collapsed_body_at(row) {
            let header = body.start - 1;
            if self.doc.cursor > previous && body.end < self.line_count() {
                self.set_cursor(self.first_nonblank(body.end));
            } else if self.mode == Mode::Normal
                && let Some((open, _)) = self.doc.folded_braces(header)
            {
                self.set_cursor(open);
                self.folded_closing_caret = Some(open);
            } else {
                self.set_cursor(self.first_nonblank(header));
            }
        }
        self.doc
            .update_search_fold_row(self.doc.location(self.doc.cursor).0);
        if edited || !self.fold_cursor_on_closing_brace() {
            self.folded_closing_caret = None;
        }
    }

    /// Open the actual closing-brace line when an edit targets the visible preview brace.
    pub(super) fn edit_fold_closing_brace(&mut self) {
        if !self.fold_cursor_on_closing_brace() {
            return;
        }
        let row = self.doc.location(self.doc.cursor).0;
        if let Some((_, close)) = self.doc.folded_braces(row) {
            let closing_row = self.doc.location(close).0;
            self.doc.reveal_row(closing_row);
            self.set_cursor(close);
        }
    }

    /// Handle non-motion folding `key` with its optional Normal-mode `count`.
    pub(super) fn fold_key(&mut self, key: &str, count: Option<usize>) -> bool {
        let row = self.doc.location(self.doc.cursor).0;
        let last = self.line_count() - 1;
        let selection = self.mode.is_visual().then(|| {
            let anchor = self.doc.location(self.anchor).0;
            row.min(anchor)..row.max(anchor) + 1
        });
        let lines = count.unwrap_or(1);
        let last_visual = selection.is_some();
        let range = selection.unwrap_or(row..row + 1);
        let changed = match key {
            "zf" | "zF" => {
                if last_visual {
                    self.close_manual_range(range.clone());
                } else if key == "zf" {
                    self.pending = Some(Pending::Operator(Operator::new(FOLD_OPERATOR, count)));
                    return true;
                } else {
                    self.close_manual_range(row..row.saturating_add(lines - 1).min(last) + 1);
                }
                true
            }
            "za" | "zA" | "zc" | "zC" | "zo" | "zO" => {
                let rows = if key == "za" {
                    row..row + 1
                } else {
                    range.clone()
                };
                self.change_fold_selection(key, rows, lines)
            }
            "zd" | "zD" => self.doc.delete_fold_range(range.clone(), key == "zD"),
            "zE" => {
                self.doc.delete_folds();
                true
            }
            "zi" | "zn" | "zN" => {
                let enabled = match key {
                    "zn" => Some(false),
                    "zN" => Some(true),
                    _ => None,
                };
                self.doc.set_folding(enabled);
                true
            }
            "zm" | "zM" | "zr" | "zR" => {
                let level = match key {
                    "zm" => FoldLevel::Change(-(lines as isize)),
                    "zr" => FoldLevel::Change(lines as isize),
                    "zM" => FoldLevel::CloseAll,
                    _ => FoldLevel::OpenAll,
                };
                self.doc.change_fold_level(level);
                true
            }
            "zv" => {
                self.doc.open_folds_at(row);
                true
            }
            "zx" | "zX" => {
                self.doc.reset_folds();
                if key == "zx" {
                    self.doc.open_folds_at(row);
                }
                true
            }
            _ => return false,
        };
        if changed {
            if last_visual && matches!(key, "zA" | "zc" | "zC" | "zo" | "zO" | "zd" | "zD") {
                self.mode = Mode::Normal;
                if matches!(key, "zc" | "zC" | "zo" | "zO" | "zD") {
                    self.set_cursor(self.doc.starts[range.start]);
                }
            }
            self.move_out_of_hidden_fold();
        } else {
            self.message = i18n::text("editor.no_fold").into();
        }
        true
    }

    /// Change all folds touched by `rows`, using `count` levels outside Visual mode.
    fn change_fold_selection(&mut self, key: &str, rows: Range<usize>, count: usize) -> bool {
        let change = match key {
            "za" | "zA" => FoldChange::Toggle,
            "zc" | "zC" => FoldChange::Close,
            _ => FoldChange::Open,
        };
        self.doc
            .change_fold_range(rows, change, count, matches!(key, "zA" | "zC" | "zO"))
    }

    /// Resolve counted fold navigation for `key` as an exclusive Vim motion.
    pub(super) fn fold_motion(&mut self, key: &str, count: Option<usize>) -> Option<Motion> {
        let edge = match key {
            "zj" => FoldEdge::Next,
            "zk" => FoldEdge::Previous,
            "[z" => FoldEdge::Start,
            "]z" => FoldEdge::End,
            _ => return None,
        };
        let row = self.doc.location(self.doc.cursor).0;
        let target = self.doc.fold_edge(row, edge, count.unwrap_or(1))?;
        self.desired_column = None;
        let mut motion = Motion::new(self.doc.starts[target], Kind::Exclusive);
        motion.jump = true;
        Some(motion)
    }

    /// Create a manual fold from a resolved half-open byte `range`.
    pub(super) fn fold_operator(&mut self, range: Range<usize>) {
        let first = self.doc.location(range.start).0;
        let end = if range.is_empty() {
            range.end
        } else {
            self.doc.previous(range.end)
        };
        let last = self.doc.location(end).0.min(self.line_count() - 1);
        self.close_manual_range(first..last + 1);
    }

    /// Create a fold covering `rows` and leave the caret on its visible header.
    fn close_manual_range(&mut self, rows: Range<usize>) {
        self.mode = Mode::Normal;
        self.doc.add_fold(rows);
        self.move_out_of_hidden_fold();
    }

    /// Toggle one fold at the caret, also used by the editor's Ex fold command.
    pub fn toggle_fold(&mut self) -> bool {
        let row = self.doc.location(self.doc.cursor).0;
        let changed = self.doc.toggle_fold(row);
        if changed {
            self.move_out_of_hidden_fold();
        }
        changed
    }

    /// Keep the native caret on a visible row after changing fold visibility.
    fn move_out_of_hidden_fold(&mut self) {
        let row = self.doc.location(self.doc.cursor).0;
        let visible = self.doc.document_row(self.doc.display_row(row));
        if visible != row {
            self.set_cursor(self.doc.starts[visible]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Use identifiable rows and a blank unnamed register for fold command tests.
    fn editor() -> (Vim, Register) {
        (
            Vim::new(Document::new(
                "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight",
            )),
            Register::default(),
        )
    }

    /// Nested folds retain independent states and can be opened or deleted by level.
    #[test]
    fn fold_commands_restore_nested_visibility() {
        let (mut vim, mut register) = editor();
        vim.text("zf3jzR2G2zFggzc", &mut register);
        assert_eq!(vim.doc.display_rows(), 5);
        vim.text("zo", &mut register);
        assert_eq!(vim.doc.display_rows(), 7);
        assert!(vim.doc.folded_at(1));
        vim.text("zMza", &mut register);
        assert!(vim.doc.folded_at(1));
        vim.text("zMzA", &mut register);
        assert_eq!(vim.doc.display_rows(), 8);
        vim.text("zCzn", &mut register);
        assert_eq!(vim.doc.display_rows(), 8);
        vim.text("zN", &mut register);
        assert_eq!(vim.doc.display_rows(), 5);
        vim.text("zOzmzr", &mut register);
        assert_eq!(vim.doc.display_rows(), 7);
        vim.text("zXzx", &mut register);
        assert_eq!(vim.doc.display_rows(), 7);
        vim.text("zd", &mut register);
        assert_eq!(vim.doc.display_rows(), 8);
        vim.text("2Gzc", &mut register);
        assert!(vim.doc.folded_at(1));
        vim.text("zDzE", &mut register);
        let revision = vim.doc.revision;
        vim.text("zM", &mut register);
        assert_eq!(vim.doc.display_rows(), 8);
        assert_eq!(revision, 0);
    }

    /// Visual and operator-pending folds share the same ranges as counted fold navigation.
    #[test]
    fn fold_motions_and_visual_creation_preserve_line_boundaries() {
        let (mut vim, mut register) = editor();
        vim.text("2GV2jzfzR6G2zFzRgg2zj", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (5, 0));
        vim.text("zk", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (3, 0));
        vim.text("[z", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (1, 0));
        vim.text("]z", &mut register);
        assert_eq!(vim.doc.location(vim.doc.cursor), (3, 0));
        vim.text("ggdzj", &mut register);
        assert_eq!(vim.doc.line(0), "two");
        vim.text("zEgghzfG", &mut register);
        assert_eq!(vim.doc.display_rows(), 1);
    }

    /// Provider refresh and text history preserve manual folds and release stale provider ranges.
    #[test]
    fn fold_sources_survive_provider_removal_and_line_edits() {
        let (mut vim, mut register) = editor();
        vim.text("3G2zF", &mut register);
        vim.doc.set_provider_folds(vec![5..8], false);
        vim.text("6Gza", &mut register);
        assert!(vim.doc.folded_at(2));
        assert!(vim.doc.folded_at(5));
        vim.doc.set_provider_folds(vec![5..8], true);
        assert!(vim.doc.folded_at(5));
        vim.doc.replace(0..0, "prefix\n", false);
        assert!(vim.doc.folded_at(3));
        assert!(vim.doc.folded_at(6));
        vim.doc.set_provider_folds(vec![6..9], true);
        assert!(vim.doc.folded_at(6));
        vim.doc.set_provider_folds(Vec::new(), false);
        assert!(vim.doc.folded_at(3));
        assert!(!vim.doc.folded_at(6));
        vim.doc.replace(0..0, "new\n", true);
        assert!(vim.doc.folded_at(4));
        vim.doc.undo(false);
        assert!(vim.doc.folded_at(3));
        vim.doc.undo(true);
        assert!(vim.doc.folded_at(4));
    }
}
