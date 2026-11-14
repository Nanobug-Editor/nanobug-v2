//! Version-bound language results exchanged with the editor.

use crate::{Diagnostic, Edit};
use serde::{Deserialize, Serialize};

/// An end-exclusive UTF-8 byte range in the current document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ByteRange {
    /// First included byte.
    pub start: usize,
    /// First byte after the range.
    pub end: usize,
}

/// Foreground roles resolved by the host's current theme.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxKind {
    Keyword,
    Type,
    Function,
    String,
    Constant,
    Comment,
    Attribute,
}

/// One non-overlapping syntax span in the document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Highlight {
    /// Span in the active document.
    pub range: ByteRange,
    /// Foreground role for text within the span.
    pub kind: SyntaxKind,
}

/// A declaration and the UTF-8 byte position of its name.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Symbol {
    /// Display name from the source.
    pub name: String,
    /// Short language-specific declaration kind.
    pub kind: String,
    /// Byte position to reveal when the symbol is selected.
    pub offset: usize,
}

/// Syntax data derived from the exact document passed to the provider.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentFeatures {
    /// Foreground spans for the current document.
    pub highlights: Vec<Highlight>,
    /// Multiline ranges whose first line stays visible when collapsed.
    pub folds: Vec<ByteRange>,
    /// Declarations available in the outline.
    pub symbols: Vec<Symbol>,
}

/// A display label and a single replacement in the completion request's document.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Completion {
    /// Text displayed in the completion list.
    pub label: String,
    /// Replacement range containing the cursor supplied in the request.
    pub edit: Edit,
}

/// A symbol declaration in a named file, using LSP's zero-based UTF-16 position.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Definition {
    pub path: std::path::PathBuf,
    pub line: u32,
    pub character: u32,
}

/// One named Tree-sitter capture in the supplied snapshot.
#[derive(Debug, Deserialize, Serialize)]
pub struct Capture {
    /// Query capture name without its leading @.
    pub name: String,
    /// Range in the parsed UTF-8 snapshot.
    pub range: ByteRange,
}

/// Bounded Tree-sitter query result with syntax recovery errors from the same tree.
#[derive(Debug, Deserialize, Serialize)]
pub struct SyntaxQuery {
    /// Identity of the opened document.
    pub opening: u64,
    /// Revision supplied to the parser.
    pub revision: u64,
    /// Query captures in source order.
    pub captures: Vec<Capture>,
    /// Missing and unexpected syntax nodes from the same tree.
    pub diagnostics: Vec<Diagnostic>,
    /// False if the capture or diagnostics budget was exhausted.
    pub complete: bool,
}

/// Authorized ancestor reads, with project-relative paths normalized to forward slashes.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectAncestors {
    /// Active document relative to the opened project root, if it has a name within that root.
    pub document: Option<String>,
    /// Available UTF-8 files, ordered from the nearest directory to the project root.
    pub files: Vec<AncestorFile>,
}

/// An ancestor file read for inspection.
#[derive(Debug, Deserialize, Serialize)]
pub struct AncestorFile {
    /// Project-relative path with slash separators.
    pub path: String,
    /// UTF-8 text with any leading BOM removed.
    pub text: String,
}
