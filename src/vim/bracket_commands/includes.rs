//! Identifier and macro lookup through quoted and local C-family includes.

use super::{Action, Vim};
use crate::{document::Document, i18n, vim::pattern};
use std::{
    collections::HashSet,
    ops::{ControlFlow, Range},
    path::{Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;

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
    /// Byte column on the matching source line.
    column: usize,
    /// A copy of the matching line for display.
    line: String,
}

/// Bound and deduplicate a traversal of the active document and its local includes.
struct Includes<'a> {
    /// Canonical files already visited, including the original document if saved.
    visited: HashSet<PathBuf>,
    /// Number of file bytes left in the search budget.
    remaining: u64,
    /// Workspace directory and `path` for explicitly addressed Ex searches.
    search: Option<(&'a Path, &'a str)>,
}

/// Match exact bracket lookup terms on the lines visited by an include traversal.
struct Lookup<'a> {
    /// Identifier to match as a separate token.
    word: &'a str,
    /// Whether to look up macro definitions instead of general occurrences.
    defines: bool,
    /// Whether to skip lines starting with a comment.
    ignore_comments: bool,
}

/// How an include or macro Ex command presents its matches.
#[derive(Clone, Copy)]
enum IncludeAction {
    Show,
    List,
    Jump,
    Split,
    Preview,
}

impl Vim {
    /// Run an include command in the addressed root rows and in all reached local headers.
    pub(in crate::vim) fn ex_include_command(
        &mut self,
        name: &str,
        args: &str,
        rows: Option<Range<usize>>,
    ) -> Option<Action> {
        let is = |full: &str, minimum| name.len() >= minimum && full.starts_with(name);
        let (bang, args) = args
            .strip_prefix('!')
            .map_or((false, args), |args| (true, args));
        let args = args.trim();
        if is("checkpath", 6) {
            if !args.is_empty() || rows.is_some() {
                self.pattern_error("Invalid :checkpath argument");
                return Some(Action::None);
            }
            let mut output = Vec::new();
            let path = self.file_search_path();
            Includes::new(&self.doc)
                .with_search(self.completion_directory(), &path)
                .check_paths(&self.doc, bang, &mut output);
            return Some(if output.is_empty() {
                self.message = i18n::text("vim_bracket.paths_found").into();
                Action::None
            } else {
                Action::ShowListing(output.join("\n"))
            });
        }
        let (mode, defines) = if is("dsearch", 2) {
            (IncludeAction::Show, true)
        } else if is("dlist", 2) {
            (IncludeAction::List, true)
        } else if is("djump", 2) {
            (IncludeAction::Jump, true)
        } else if is("dsplit", 3) {
            (IncludeAction::Split, true)
        } else if is("isearch", 2) {
            (IncludeAction::Show, false)
        } else if is("ilist", 2) {
            (IncludeAction::List, false)
        } else if is("ijump", 2) {
            (IncludeAction::Jump, false)
        } else if is("isplit", 3) {
            (IncludeAction::Split, false)
        } else if is("psearch", 2) {
            (IncludeAction::Preview, false)
        } else {
            return None;
        };
        Some(self.run_ex_include(mode, defines, bang, args, rows))
    }

    /// Search the include tree using the Ex pattern and dispatch the selected match.
    fn run_ex_include(
        &mut self,
        mode: IncludeAction,
        defines: bool,
        bang: bool,
        args: &str,
        rows: Option<Range<usize>>,
    ) -> Action {
        let (count, query) =
            args.split_once(char::is_whitespace)
                .map_or((1, args), |(first, tail)| {
                    first
                        .parse::<usize>()
                        .map_or((1, args), |count| (count, tail.trim_start()))
                });
        let (query, delimited) = if let Some(query) = query.strip_prefix('/') {
            let Some((query, tail)) = pattern::delimited(query, '/') else {
                self.pattern_error("Missing include search delimiter");
                return Action::None;
            };
            if !tail.trim().is_empty() {
                self.pattern_error("Trailing include search argument");
                return Action::None;
            }
            (query, true)
        } else {
            (query.to_owned(), false)
        };
        if count == 0 || query.is_empty() {
            self.pattern_error("Expected an include search pattern");
            return Action::None;
        }
        let case = self.pattern_case(&query, None);
        let regex = if defines {
            None
        } else {
            let source = if delimited {
                query.clone()
            } else {
                format!(r"\<{query}\>")
            };
            match pattern::compile(&source, case) {
                Ok(regex) => Some(regex),
                Err(error) => {
                    self.pattern_error(&error);
                    return Action::None;
                }
            }
        };
        let lookup = Lookup {
            word: &query,
            defines: true,
            ignore_comments: false,
        };
        let mut found = Vec::new();
        let limit = if matches!(mode, IncludeAction::List) {
            MAX_OUTPUT
        } else {
            count
        };
        let rows = rows.unwrap_or(0..self.line_count());
        let path = self.file_search_path();
        let _ = Includes::new(&self.doc)
            .with_search(self.completion_directory(), &path)
            .visit_rows(&self.doc, rows, &mut |doc, row| {
                let line = doc.line(row);
                let column = if let Some(regex) = &regex {
                    let ignore = !bang
                        && ["//", "/*", "*"]
                            .iter()
                            .any(|prefix| line.trim_start().starts_with(prefix));
                    (!ignore)
                        .then(|| regex.find(line))
                        .flatten()
                        .map(|m| m.start())
                } else {
                    lookup.match_column(line, doc.starts[row], None)
                };
                if let Some(column) = column {
                    found.push(IncludeMatch {
                        path: doc.path.clone(),
                        row,
                        offset: doc.starts[row] + column,
                        column,
                        line: line.to_owned(),
                    });
                }
                if found.len() >= limit {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
        if matches!(mode, IncludeAction::List) && !found.is_empty() {
            return Action::ShowListing(
                found
                    .iter()
                    .map(IncludeMatch::display)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        let Some(item) = found.into_iter().nth(count - 1) else {
            self.ex_failed = true;
            self.message = i18n::format("vim_bracket.no_match", &[("word", &query)]);
            return Action::None;
        };
        match mode {
            IncludeAction::Show | IncludeAction::List => self.message = item.display(),
            IncludeAction::Jump if item.path == self.doc.path => self.jump_to(item.offset),
            IncludeAction::Jump => {
                if let Some(path) = item.path {
                    return Action::OpenFile(
                        path.to_string_lossy().into_owned(),
                        Some((item.row, item.column)),
                    );
                }
            }
            IncludeAction::Split | IncludeAction::Preview => {
                return Action::IncludeSplit {
                    path: item
                        .path
                        .filter(|path| self.doc.path.as_ref() != Some(path)),
                    row: item.row,
                    column: item.column,
                    preview: matches!(mode, IncludeAction::Preview),
                };
            }
        }
        Action::None
    }

    /// Run the bracket include \`command\` using its optional \`count\`.
    /// Control variants jump to the chosen match. Uppercase variants list matches.
    pub(super) fn include_lookup(&mut self, command: &str, count: Option<usize>) -> Action {
        let Some(range) = self.keyword() else {
            return Action::None;
        };
        let word = &self.doc.text[range.clone()];
        let list = command.ends_with(['I', 'D']);
        let jump = command.contains("Ctrl+") || command.ends_with("Tab");
        let matches = self.include_matches(command, count, word, range.start);
        if list && !matches.is_empty() {
            self.message = matches
                .iter()
                .map(IncludeMatch::display)
                .collect::<Vec<_>>()
                .join(" | ");
        } else if let Some(item) = matches
            .into_iter()
            .nth(count.unwrap_or(1).saturating_sub(1))
        {
            if jump {
                if item.path == self.doc.path {
                    self.jump_to(item.offset);
                } else if let Some(path) = item.path {
                    return Action::OpenFile(
                        path.to_string_lossy().into_owned(),
                        Some((item.row, item.column)),
                    );
                }
            } else {
                self.message = item.display();
            }
        } else {
            self.message = i18n::format("vim_bracket.no_match", &[("word", &word)]);
        }
        Action::None
    }

    /// Find the `count`th local include match for a new split without moving this caret.
    /// When `defines` is true, restrict the search to macro definitions.
    /// Return the path (absent for this buffer), zero-based row and UTF-8 byte column.
    pub fn split_include_target(
        &self,
        count: Option<usize>,
        defines: bool,
    ) -> Option<(Option<PathBuf>, usize, usize)> {
        let range = self.keyword()?;
        let word = &self.doc.text[range.clone()];
        let result = self
            .include_matches(
                if defines { "[Ctrl+d" } else { "[Ctrl+i" },
                count,
                word,
                range.start,
            )
            .into_iter()
            .nth(count.unwrap_or(1).saturating_sub(1))?;
        Some((
            result
                .path
                .filter(|path| self.doc.path.as_ref() != Some(path)),
            result.row,
            result.column,
        ))
    }

    /// Collect matches for include `command` and `count`, excluding this `word` at byte `cursor`.
    fn include_matches(
        &self,
        command: &str,
        count: Option<usize>,
        word: &str,
        cursor: usize,
    ) -> Vec<IncludeMatch> {
        let start = if command.starts_with(']') {
            self.doc.location(self.doc.cursor).0 + 1
        } else {
            0
        };
        let list = command.ends_with(['I', 'D']);
        let lookup = Lookup {
            word,
            defines: matches!(&command[1..], "d" | "D" | "Ctrl+d"),
            ignore_comments: !list && count.is_none(),
        };
        let limit = if list { MAX_OUTPUT } else { count.unwrap_or(1) };
        let mut matches = Vec::new();
        let _ = Includes::new(&self.doc).visit(&self.doc, start, &mut |doc, row| {
            let original = std::ptr::eq(doc, &self.doc).then_some(cursor);
            let line = doc.line(row);
            if let Some(column) = lookup.match_column(line, doc.starts[row], original) {
                matches.push(IncludeMatch {
                    path: doc.path.clone(),
                    row,
                    offset: doc.starts[row] + column,
                    column,
                    line: line.to_owned(),
                });
            }
            if matches.len() >= limit {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        matches
    }

    /// Complete `prefix` using local include identifiers or only defines, omitting the word at `cursor`.
    pub(in crate::vim) fn include_completion(
        &self,
        prefix: &str,
        cursor: usize,
        defines: bool,
    ) -> Vec<String> {
        let mut values = Vec::new();
        let mut seen = HashSet::new();
        let _ = Includes::new(&self.doc).visit(&self.doc, 0, &mut |doc, row| {
            let line = doc.line(row);
            let (offset, words) = if defines {
                let Some(tail) = directive_tail(line, "define").map(str::trim_start) else {
                    return ControlFlow::Continue(());
                };
                (
                    line.len() - tail.len(),
                    &tail[..tail.find(|ch| !identifier(ch)).unwrap_or(tail.len())],
                )
            } else {
                (0, line)
            };
            for (at, word) in words.unicode_word_indices() {
                let start = doc.starts[row] + offset + at;
                let current =
                    std::ptr::eq(doc, &self.doc) && start < cursor && cursor <= start + word.len();
                if !current
                    && !word.is_empty()
                    && word != prefix
                    && word.starts_with(prefix)
                    && seen.insert(word.to_owned())
                {
                    values.push(word.to_owned());
                    if values.len() == 100 {
                        return ControlFlow::Break(());
                    }
                }
            }
            ControlFlow::Continue(())
        });
        values
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

impl<'a> Includes<'a> {
    /// Start a bounded traversal with `document` already registered as visited.
    fn new(document: &Document) -> Self {
        Self {
            visited: document.path.iter().cloned().collect(),
            remaining: MAX_BYTES,
            search: None,
        }
    }

    /// Resolve Ex includes through both their source directory and the configured path.
    fn with_search(mut self, directory: &'a Path, options: &'a str) -> Self {
        self.search = Some((directory, options));
        self
    }

    /// Visit each source row of `document` from `start`, recursively following includes before continuing.
    fn visit(
        &mut self,
        document: &Document,
        start: usize,
        visit_line: &mut impl FnMut(&Document, usize) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.visit_rows(document, start..document.starts.len(), visit_line)
    }

    /// Restrict only the starting document to `rows`, following reached headers in order.
    fn visit_rows(
        &mut self,
        document: &Document,
        rows: Range<usize>,
        visit_line: &mut impl FnMut(&Document, usize) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        for row in rows {
            let line = document.line(row);
            if let Some(name) = include_name(line) {
                if let Some(include) = self
                    .resolve_include(document, name)
                    .and_then(|path| self.open_include(path))
                {
                    self.visit(&include, 0, visit_line)?;
                }
            } else {
                visit_line(document, row)?;
            }
        }
        ControlFlow::Continue(())
    }

    /// Report missing reachable include paths, optionally listing present headers as well.
    fn check_paths(&mut self, document: &Document, all: bool, output: &mut Vec<String>) {
        for row in 0..document.starts.len() {
            if output.len() >= MAX_OUTPUT {
                break;
            }
            let Some(name) = include_name(document.line(row)) else {
                continue;
            };
            if let Some(path) = self.resolve_include(document, name) {
                if all {
                    output.push(path.display().to_string());
                }
                if let Some(header) = self.open_include(path) {
                    self.check_paths(&header, all, output);
                }
            } else {
                let owner = document
                    .path
                    .as_ref()
                    .map_or("[No Name]".into(), |path| path.display().to_string());
                output.push(format!("{owner}:{} {name}", row + 1));
            }
        }
    }

    /// Choose the first existing path for an include operand named in `document`.
    fn resolve_include(&self, document: &Document, name: &str) -> Option<PathBuf> {
        let current = document.path.as_deref();
        if let Some(path) = current
            .and_then(Path::parent)
            .map(|dir| dir.join(name))
            .filter(|path| path.is_file())
        {
            return Some(path);
        }
        let (directory, options) = self.search?;
        crate::vim::ex_files::search::find_file(directory, current, options, name, 1)
    }

    /// Read `path` if it is a new local include within the shared file and byte budgets.
    fn open_include(&mut self, path: PathBuf) -> Option<Document> {
        if self.visited.len() >= MAX_FILES || self.remaining == 0 {
            return None;
        }
        let path = path.canonicalize().ok()?;
        if !self.visited.insert(path.clone()) {
            return None;
        }
        let metadata = path.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > self.remaining {
            return None;
        }
        self.remaining -= metadata.len();
        Document::open(&path).ok()
    }
}

impl Lookup<'_> {
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

    #[test]
    fn ex_include_scans_ranges_and_routes_displays_jumps_splits_and_preview() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("main.c");
        let header = dir.path().join("local.h");
        std::fs::write(
            &root,
            "#include \"local.h\"\n#include \"missing.h\"\nint local_target = 3;\n",
        )
        .unwrap();
        std::fs::write(&header, "#define LOCAL 42\nint local_target = 2;\n").unwrap();
        let mut vim = Vim::new(Document::open(&root).unwrap());
        let mut register = crate::vim::Register::default();
        let location = header.canonicalize().unwrap();
        for cmd in ["dsearch /LOCAL/", "isearch 2 local_target"] {
            assert_eq!(
                vim.execute_ex_line(cmd, &mut register),
                Action::None,
                "{cmd}"
            );
            assert!(
                vim.message
                    .contains(if cmd.starts_with('d') { "42" } else { "3;" })
            );
        }
        for cmd in ["dlist LOCAL", "ilist /local_.*/", "checkpath", "checkpath!"] {
            assert!(
                matches!(vim.execute_ex_line(cmd, &mut register), Action::ShowListing(output) if !output.is_empty()),
                "{cmd}"
            );
        }
        for (cmd, row, column) in [("djump LOCAL", 0, 8), ("1ijump local_target", 1, 4)] {
            assert_eq!(
                vim.execute_ex_line(cmd, &mut register),
                Action::OpenFile(location.to_string_lossy().into_owned(), Some((row, column))),
                "{cmd}"
            );
        }
        for (cmd, row, column, preview) in [
            ("dsplit LOCAL", 0, 8, false),
            ("isplit local_target", 1, 4, false),
            ("psearch local_target", 1, 4, true),
        ] {
            assert_eq!(
                vim.execute_ex_line(cmd, &mut register),
                Action::IncludeSplit {
                    path: Some(location.clone()),
                    row,
                    column,
                    preview
                },
                "{cmd}"
            );
        }
        assert_eq!(
            vim.execute_ex_line("3ijump local_target", &mut register),
            Action::None
        );
        assert_eq!(vim.doc.location(vim.doc.cursor), (2, 4));
    }
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
            Action::OpenFile(
                header
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                Some((1, 8)),
            ),
        );
        vim.text("[I", &mut register);
        assert!(vim.message.contains("header.h:2"));
    }
}
