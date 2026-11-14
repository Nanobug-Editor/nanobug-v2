//! Identifier and macro lookup through quoted and local C-family includes.

use super::{Action, Vim};
use crate::{document::Document, i18n};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Bound the file reads and displayed rows in a single include search.
const MAX_FILES: usize = 64;
/// Bound the total bytes read from included files.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Keep the command's retained output reasonably sized.
const MAX_OUTPUT: usize = 200;

/// A match in the active document or one of the included files.
struct IncludeMatch {
    /// Absent for an active document without a filename.
    path: Option<PathBuf>,
    /// Zero-based line and UTF-8 byte location of the identifier.
    row: usize,
    offset: usize,
    /// A copy of the matching line for display.
    line: String,
}

/// Shared traversal state, retaining a match limit across nested includes.
struct Lookup<'a> {
    /// Identifier to match as a separate token.
    word: &'a str,
    /// Whether to look up macro definitions instead of general occurrences.
    defines: bool,
    /// Whether to skip lines starting with a comment.
    ignore_comments: bool,
    /// Maximum number of matches needed by this command.
    limit: usize,
    /// Settled matches in search order.
    matches: Vec<IncludeMatch>,
    /// Canonical files already visited, including the original document if saved.
    visited: HashSet<PathBuf>,
    /// Number of file bytes left in the search budget.
    remaining: u64,
}

impl Vim {
    /// Run the bracket include \`command\` using its optional \`count\`.
    /// Control variants jump to the chosen match. Uppercase variants list matches.
    pub(super) fn include_lookup(&mut self, command: &str, count: Option<usize>) -> Action {
        let Some(range) = self.keyword() else {
            return Action::None;
        };
        let word = &self.doc.text[range.clone()];
        let list = command.ends_with(['I', 'D']);
        let jump = command.contains("Ctrl+") || command.ends_with("Tab");
        let start = if command.starts_with(']') {
            self.doc.location(self.doc.cursor).0 + 1
        } else {
            0
        };
        let mut lookup = Lookup {
            word,
            defines: matches!(&command[1..], "d" | "D" | "Ctrl+d"),
            ignore_comments: !list && count.is_none(),
            limit: if list { MAX_OUTPUT } else { count.unwrap_or(1) },
            matches: Vec::new(),
            visited: self.doc.path.iter().cloned().collect(),
            remaining: MAX_BYTES,
        };
        lookup.search(&self.doc, start, Some(range.start));
        if list && !lookup.matches.is_empty() {
            self.message = lookup
                .matches
                .iter()
                .map(IncludeMatch::display)
                .collect::<Vec<_>>()
                .join(" | ");
        } else if let Some(item) = lookup
            .matches
            .into_iter()
            .nth(count.unwrap_or(1).saturating_sub(1))
        {
            if jump {
                if item.path == self.doc.path {
                    self.jump_to(item.offset);
                } else if let Some(path) = item.path {
                    return Action::OpenAt(path.to_string_lossy().into_owned(), item.row + 1);
                }
            } else {
                self.message = item.display();
            }
        } else {
            self.message = i18n::format("vim_bracket.no_match", &[("word", &word)]);
        }
        Action::None
    }
}

impl IncludeMatch {
    /// Format the source file, line number and matching source text for status output.
    fn display(&self) -> String {
        self.path.as_deref().and_then(Path::file_name).map_or_else(
            || format!("{} {}", self.row + 1, self.line.trim()),
            |name| {
                format!(
                    "{}:{} {}",
                    name.to_string_lossy(),
                    self.row + 1,
                    self.line.trim()
                )
            },
        )
    }
}

impl Lookup<'_> {
    /// Search \`document\` from \`start\`, skipping the original occurrence at \`cursor\`.
    fn search(&mut self, document: &Document, start: usize, cursor: Option<usize>) {
        for row in start..document.starts.len() {
            if self.matches.len() >= self.limit {
                break;
            }
            let line = document.line(row);
            if let Some(name) = include_name(line)
                && let Some(parent) = document.path.as_deref().and_then(Path::parent)
            {
                self.search_include(parent.join(name));
            } else if let Some(column) = self.match_column(line, document.starts[row], cursor) {
                self.matches.push(IncludeMatch {
                    path: document.path.clone(),
                    row,
                    offset: document.starts[row] + column,
                    line: line.to_owned(),
                });
            }
        }
    }

    /// Read one local include \`path\`, sharing byte and file budgets with its ancestors.
    fn search_include(&mut self, path: PathBuf) {
        if self.visited.len() >= MAX_FILES || self.remaining == 0 {
            return;
        }
        let Ok(path) = path.canonicalize() else {
            return;
        };
        if !self.visited.insert(path.clone()) {
            return;
        }
        let Ok(metadata) = path.metadata() else {
            return;
        };
        if !metadata.is_file() || metadata.len() > self.remaining {
            return;
        }
        self.remaining -= metadata.len();
        if let Ok(document) = Document::open(&path) {
            self.search(&document, 0, None);
        }
    }

    /// Return the identifier column in \`line\`, applying macro/comment and cursor constraints.
    fn match_column(&self, line: &str, start: usize, cursor: Option<usize>) -> Option<usize> {
        if self.defines {
            let tail = directive_tail(line, "define")?.trim_start();
            return (tail.starts_with(self.word)
                && !tail[self.word.len()..].starts_with(identifier))
            .then(|| line.len() - tail.len());
        }
        if self.ignore_comments
            && ["//", "/*", "*"]
                .iter()
                .any(|prefix| line.trim_start().starts_with(prefix))
        {
            return None;
        }
        line.match_indices(self.word).find_map(|(column, matched)| {
            let end = column + matched.len();
            ((cursor != Some(start + column))
                && !line[..column].ends_with(identifier)
                && !line[end..].starts_with(identifier))
            .then_some(column)
        })
    }
}

/// Return the remainder of a matching preprocessor \`directive\` in \`line\`.
fn directive_tail<'a>(line: &'a str, directive: &str) -> Option<&'a str> {
    let tail = line
        .trim_start()
        .strip_prefix('#')?
        .trim_start()
        .strip_prefix(directive)?;
    (!tail.starts_with(identifier)).then_some(tail)
}

/// Read a quoted or angle-bracket include path written on \`line\`.
fn include_name(line: &str) -> Option<&str> {
    let name = directive_tail(line, "include")?.trim_start();
    if let Some(name) = name.strip_prefix('"') {
        name.split_once('"').map(|(name, _)| name)
    } else {
        name.strip_prefix('<')?
            .split_once('>')
            .map(|(name, _)| name)
    }
}

/// Whether \`ch\` can extend the current identifier in lookup results.
fn identifier(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::Register;
    use std::fs;

    /// Check that native bracket actions use the active document and resolve local includes.
    #[test]
    fn bracket_lookup_follows_local_includes_and_can_jump_to_keywords() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.c");
        let header = directory.path().join("header.h");
        fs::write(
            &source,
            "#include \"header.h\"\nint local_name;\nint main() { return macro_name + local_name; }\n",
        )
        .unwrap();
        fs::write(&header, "#include \"main.c\"\n#define macro_name 42\n").unwrap();
        let mut vim = Vim::new(Document::open(&source).unwrap());
        let mut register = Register::default();
        vim.set_cursor(vim.doc.text.rfind("local_name").unwrap());
        vim.key("[", &mut register);
        assert_eq!(vim.key("Ctrl+i", &mut register), Action::None);
        assert_eq!(vim.doc.location(vim.doc.cursor), (1, 4));
        vim.set_cursor(vim.doc.text.find("macro_name").unwrap());
        vim.key("[", &mut register);
        assert_eq!(
            vim.key("Ctrl+d", &mut register),
            Action::OpenAt(
                header
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                2
            ),
        );
        vim.text("[I", &mut register);
        assert!(vim.message.contains("header.h:2"));
    }
}
