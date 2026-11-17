//! Line comment toggling and comment text objects for native Vim operators.

use super::{Mode, Vim};
use crate::i18n;
use std::{ops::Range, path::Path};

/// Internal operator identity for `gc`.
pub(super) const COMMENT_OPERATOR: char = '\u{8}';

/// Delimiters before and after the text of one commented line.
#[derive(Clone, Copy)]
struct CommentStyle {
    /// Opening delimiter, including its preferred spacing.
    prefix: &'static str,
    /// Closing delimiter and spacing for languages that require it.
    suffix: &'static str,
}

impl CommentStyle {
    /// Resolve a supported comment style from a file's path.
    fn for_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        let ext = path
            .extension()
            .unwrap_or_default()
            .to_str()?
            .to_ascii_lowercase();
        let (prefix, suffix) = match name.as_str() {
            "dockerfile" | "makefile" | "gnumakefile" | "cmakelists.txt" | "meson.build"
            | ".env" | ".gitignore" | ".gitattributes" | ".editorconfig" => ("# ", ""),
            _ => match ext.as_str() {
                "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hxx" | "rs" | "go" | "js" | "mjs"
                | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "java" | "kt" | "kts" | "cs"
                | "swift" | "dart" | "zig" | "jsonc" | "scss" | "less" | "proto" | "groovy"
                | "gradle" => ("// ", ""),
                "py" | "pyw" | "rb" | "r" | "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1"
                | "toml" | "yml" | "yaml" | "cmake" | "ex" | "exs" | "nix" | "awk" | "hcl"
                | "cfg" | "env" => ("# ", ""),
                "lua" | "sql" | "hs" => ("-- ", ""),
                "css" | "php" => ("/* ", " */"),
                "html" | "htm" | "xhtml" | "xml" | "svg" | "md" | "markdown" | "vue" | "svelte" => {
                    ("<!-- ", " -->")
                }
                "ini" | "lisp" | "clj" | "scm" => ("; ", ""),
                "ml" | "mli" => ("(* ", " *)"),
                "tex" => ("% ", ""),
                "vim" => ("\"", ""),
                "bat" | "cmd" => ("REM ", ""),
                _ => return None,
            },
        };
        Some(Self { prefix, suffix })
    }

    /// Find the uncommented byte range and the end of the closing marker in `line`.
    /// Prefer exact delimiter spacing, then accept manually typed markers.
    fn interior(self, line: &str) -> Option<(Range<usize>, usize)> {
        let trimmed = line.trim_start_matches([' ', '\t']);
        let indent = line.len() - trimmed.len();
        let body = if self.suffix.is_empty() {
            trimmed
        } else {
            trimmed.trim_end_matches([' ', '\t'])
        };
        let interior = |prefix: &str, suffix: &str| {
            let middle = body.strip_prefix(prefix)?.strip_suffix(suffix)?;
            let start = indent + prefix.len();
            Some((start..start + middle.len(), indent + body.len()))
        };
        interior(self.prefix, self.suffix)
            .or_else(|| interior(self.prefix.trim(), self.suffix.trim()))
    }
}

impl Vim {
    /// Toggle the complete lines touched by `range` as one undo step, preserving their text.
    pub(super) fn toggle_comment_lines(&mut self, range: Range<usize>) {
        let (first, column) = self.doc.location(range.start);
        let last = self
            .doc
            .location(self.doc.previous(range.end).max(range.start))
            .0;
        let caret_column = if self.mode.is_visual() {
            column
        } else {
            self.doc.location(self.doc.cursor).1
        };
        self.mode = Mode::Normal;
        let Some(style) = self.doc.path.as_deref().and_then(CommentStyle::for_path) else {
            self.message = i18n::text("vim_g.no_comment_style").into();
            return;
        };
        let lines = (first..=last).map(|row| self.doc.line(row));
        let indent = lines
            .clone()
            .filter_map(|line| {
                let content = line.trim_start_matches([' ', '\t']);
                (!content.is_empty()).then(|| &line[..line.len() - content.len()])
            })
            .min_by_key(|indent| indent.len())
            .unwrap_or_default()
            .to_owned();
        let uncomment = lines
            .filter(|line| !line.trim_matches([' ', '\t']).is_empty())
            .all(|line| style.interior(line).is_some());
        let prefix = format!("{indent}{}", style.prefix);
        let blank = format!("{indent}{}{}", style.prefix.trim(), style.suffix.trim());
        let revision = self.doc.revision;
        for row in (first..=last).rev() {
            let line = self.doc.line_range(row);
            let text = &self.doc.text[line.clone()];
            let leading = text.len() - text.trim_start_matches([' ', '\t']).len();
            if uncomment {
                let Some((body, end)) = style.interior(text) else {
                    continue;
                };
                if text[body.clone()].trim_matches([' ', '\t']).is_empty() {
                    self.replace(line, "", self.doc.revision == revision);
                } else {
                    self.replace(
                        line.start + body.end..line.start + end,
                        "",
                        self.doc.revision == revision,
                    );
                    self.replace(
                        line.start + leading..line.start + body.start,
                        "",
                        self.doc.revision == revision,
                    );
                }
            } else if leading == text.len() {
                self.replace(line, &blank, self.doc.revision == revision);
            } else {
                let shared = text.starts_with(&indent);
                self.replace(
                    line.end..line.end,
                    style.suffix,
                    self.doc.revision == revision,
                );
                if shared {
                    let at = line.start + indent.len();
                    self.replace(at..at, style.prefix, self.doc.revision == revision);
                } else {
                    self.replace(
                        line.start..line.start + indent.len(),
                        &prefix,
                        self.doc.revision == revision,
                    );
                }
            }
        }
        self.set_cursor(self.doc.offset(first, caret_column));
    }

    /// Find the contiguous comment block at the caret for operator-pending `gc`.
    pub(super) fn commented_lines_at_cursor(&self) -> Option<Range<usize>> {
        let style = CommentStyle::for_path(self.doc.path.as_deref()?)?;
        let row = self.doc.location(self.doc.cursor).0;
        let commented = |row| style.interior(self.doc.line(row)).is_some();
        if !commented(row) {
            return None;
        }
        let first = (0..row)
            .rev()
            .take_while(|&row| commented(row))
            .last()
            .unwrap_or(row);
        let last = (row + 1..self.line_count())
            .take_while(|&row| commented(row))
            .last()
            .unwrap_or(row);
        Some(self.line_span(first, last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Open a named in-memory fixture so commenting uses its file type.
    fn fixture(path: &str, text: &str) -> (Vim, Register) {
        let mut doc = Document::new(text);
        doc.path = Some(path.into());
        (Vim::new(doc), Register::default())
    }

    /// Counted, motion and Visual forms use the minimum indentation and undo in one step.
    #[test]
    fn gc_toggles_counted_motion_and_visual_lines() {
        let original = "  alpha()\n    beta()\ngamma()";
        let commented = "  // alpha()\n  //   beta()\ngamma()";
        for keys in ["2gcc", "gcj", "Vjgc"] {
            let (mut vim, mut register) = fixture("example.rs", original);
            vim.set_cursor(3);
            vim.text(keys, &mut register);
            assert_eq!(vim.doc.text, commented, "{keys}");
            assert_eq!(vim.mode, Mode::Normal);
            vim.key("u", &mut register);
            assert_eq!(vim.doc.text, original, "{keys}");
        }
    }

    /// Paired delimiters retain trailing text whitespace, and pending gc is a comment object.
    #[test]
    fn gc_removes_paired_markers_and_selects_comment_blocks() {
        let (mut vim, mut register) = fixture("example.css", "  /* color: red; */  \nnext");
        vim.set_cursor(12);
        vim.text("gcc", &mut register);
        assert_eq!(vim.doc.text, "  color: red;  \nnext");
        assert_eq!(vim.doc.cursor, 2);
        vim.text("gcc", &mut register);
        assert_eq!(vim.doc.text, "  /* color: red;   */\nnext");

        let (mut vim, mut register) = fixture("example.rs", "// first\n// second\n\n// other");
        vim.set_cursor(13);
        vim.text("dgc", &mut register);
        assert_eq!(vim.doc.text, "\n// other");
    }
}
