//! Portable syntax roles, capture projection and semantic modifiers shared by language providers.

use super::{ByteRange, Capture, DocumentFeatures, Highlight, Symbol};
use serde::{Deserialize, Serialize};
use std::{cmp::Reverse, collections::BinaryHeap};

/// A role's position is local to this build. The names on the wire and in themes are stable.
macro_rules! roles {
    ($( $role:ident, $wire:literal, $capture:literal, $parent:expr, $description:literal; )+) => {
        /// Meaning of a syntax span, independent of the active palette.
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[repr(u8)]
        pub enum SyntaxKind { $( #[doc = $description] #[serde(rename = $wire)] $role, )+ }
        impl SyntaxKind {
            /// All roles, in parent-before-child order.
            pub const ALL: &'static [Self] = &[$( Self::$role, )+];
            /// Stable name used by Ex and portable settings.
            pub const fn group(self) -> &'static str { match self { $( Self::$role => stringify!($role), )+ } }
            /// Canonical Tree-sitter capture linked to the Ex group.
            pub const fn capture_group(self) -> &'static str { match self { $( Self::$role => concat!("@", $capture), )+ } }
            /// Optional sparse color key in a theme file.
            pub const fn theme_key(self) -> &'static str { match self { $( Self::$role => concat!("syntax_", $wire), )+ } }
            /// Less specific role used when the user has supplied no override.
            pub const fn parent(self) -> Option<Self> { match self { $( Self::$role => $parent, )+ } }
            /// Accept a canonical query capture, ignoring outline and fold captures.
            pub fn from_capture(capture: &str) -> Option<Self> { Some(match capture { $( $capture => Self::$role, )+ _ => return None }) }
        }
    }
}

roles! {
    Comment, "comment", "comment", None, "Comments.";
    DocComment, "doc_comment", "comment.documentation", Some(Self::Comment), "Documentation comments.";
    TodoComment, "todo_comment", "comment.todo", Some(Self::Comment), "Task markers.";
    Keyword, "keyword", "keyword", None, "Keywords.";
    Conditional, "conditional", "keyword.conditional", Some(Self::Keyword), "Conditionals.";
    Repeat, "repeat", "keyword.repeat", Some(Self::Keyword), "Loops.";
    Return, "return", "keyword.return", Some(Self::Keyword), "Returns.";
    Import, "import", "keyword.import", Some(Self::Keyword), "Imports and exports.";
    Exception, "exception", "keyword.exception", Some(Self::Keyword), "Exceptions.";
    Directive, "directive", "keyword.directive", Some(Self::Keyword), "Preprocessor directives.";
    Modifier, "modifier", "keyword.modifier", Some(Self::Keyword), "Declaration modifiers.";
    Type, "type", "type", None, "Types.";
    BuiltinType, "builtin_type", "type.builtin", Some(Self::Type), "Built-in types.";
    Class, "class", "type.class", Some(Self::Type), "Classes.";
    Struct, "struct", "type.struct", Some(Self::Type), "Structures.";
    Interface, "interface", "type.interface", Some(Self::Type), "Interfaces and traits.";
    Enum, "enum", "type.enum", Some(Self::Type), "Enum types.";
    TypeParameter, "type_parameter", "type.parameter", Some(Self::Type), "Type parameters.";
    Function, "function", "function", None, "Functions.";
    Method, "method", "function.method", Some(Self::Function), "Methods.";
    Constructor, "constructor", "constructor", Some(Self::Function), "Constructors.";
    Macro, "macro", "function.macro", Some(Self::Function), "Macros.";
    BuiltinFunction, "builtin_function", "function.builtin", Some(Self::Function), "Built-in functions.";
    String, "string", "string", None, "Strings.";
    Character, "character", "character", Some(Self::String), "Characters.";
    Regexp, "regexp", "string.regexp", Some(Self::String), "Regular expressions.";
    Escape, "escape", "string.escape", Some(Self::String), "Escape sequences.";
    Interpolation, "interpolation", "punctuation.special", Some(Self::String), "Interpolation delimiters.";
    Constant, "constant", "constant", None, "Constants.";
    Number, "number", "number", Some(Self::Constant), "Numbers.";
    Float, "float", "number.float", Some(Self::Number), "Floating-point numbers.";
    Boolean, "boolean", "boolean", Some(Self::Constant), "Booleans.";
    Null, "null", "constant.null", Some(Self::Constant), "Null values.";
    EnumMember, "enum_member", "constant.enum", Some(Self::Constant), "Enum members.";
    Attribute, "attribute", "attribute", None, "Annotations and decorators.";
    Variable, "variable", "variable", None, "Variables.";
    Parameter, "parameter", "variable.parameter", Some(Self::Variable), "Parameters.";
    Field, "field", "variable.member", Some(Self::Variable), "Member fields.";
    BuiltinVariable, "builtin_variable", "variable.builtin", Some(Self::Variable), "Built-in variables.";
    Module, "module", "module", None, "Modules and namespaces.";
    Operator, "operator", "operator", None, "Operators.";
    Punctuation, "punctuation", "punctuation", None, "Punctuation.";
    Bracket, "bracket", "punctuation.bracket", Some(Self::Punctuation), "Brackets.";
    Delimiter, "delimiter", "punctuation.delimiter", Some(Self::Punctuation), "Delimiters.";
    Lifetime, "lifetime", "lifetime", None, "Lifetimes.";
    Label, "label", "label", None, "Jump and loop labels.";
    Tag, "tag", "tag", None, "Tag names.";
    Component, "component", "tag.component", Some(Self::Tag), "Component names.";
    TagAttribute, "tag_attribute", "tag.attribute", None, "Tag attributes.";
    TemplateDirective, "template_directive", "tag.directive", Some(Self::TagAttribute), "Template directives.";
    CssSelector, "css_selector", "css.selector", None, "CSS selectors.";
    CssProperty, "css_property", "css.property", None, "CSS properties.";
    ConfigKey, "config_key", "property.key", None, "Configuration keys.";
    SqlTable, "sql_table", "sql.table", None, "Table names.";
    SqlColumn, "sql_column", "sql.column", None, "Column names.";
    SqlAlias, "sql_alias", "sql.alias", None, "Aliases.";
    SqlParameter, "sql_parameter", "sql.parameter", None, "Bound parameters.";
    MarkupHeading, "markup_heading", "markup.heading", None, "Headings.";
    MarkupEmphasis, "markup_emphasis", "markup.italic", None, "Emphasis.";
    MarkupStrong, "markup_strong", "markup.strong", None, "Strong emphasis.";
    MarkupStrike, "markup_strike", "markup.strikethrough", None, "Strikethrough.";
    MarkupLink, "markup_link", "markup.link.label", None, "Link text.";
    MarkupUrl, "markup_url", "markup.link.url", None, "Link destinations.";
    MarkupCode, "markup_code", "markup.raw.inline", None, "Inline code.";
    MarkupCodeBlock, "markup_code_block", "markup.raw.block", None, "Code blocks.";
    MarkupQuote, "markup_quote", "markup.quote", None, "Block quotes.";
    MarkupList, "markup_list", "markup.list", None, "List markers.";
    MarkupTable, "markup_table", "markup.table", None, "Table headings.";
    MarkupMarker, "markup_marker", "markup.marker", None, "Markup markers.";
    Readonly, "readonly", "modifier.readonly", None, "Read-only.";
    Static, "static", "modifier.static", None, "Static.";
    Deprecated, "deprecated", "modifier.deprecated", None, "Deprecated.";
}

/// Composable symbol properties from a language provider, encoded as a compact bit mask.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SyntaxModifiers(pub u8);

impl SyntaxModifiers {
    /// Read-only, static and deprecated flags in rendering order.
    pub const FLAGS: [(u8, SyntaxKind); 3] = [
        (1, SyntaxKind::Readonly),
        (2, SyntaxKind::Static),
        (4, SyntaxKind::Deprecated),
    ];
    /// Omit the field for plain lexical tokens and older providers.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl DocumentFeatures {
    /// Resolve a worker's named captures against its exact UTF-8 snapshot.
    pub fn from_captures(text: &str, captures: Vec<Capture>) -> Self {
        let mut result = Self::default();
        for Capture { name, range } in captures {
            let Some(fragment) = text.get(range.start..range.end).filter(|s| !s.is_empty()) else {
                continue;
            };
            if let Some(kind) = SyntaxKind::from_capture(&name) {
                result.highlights.push(Highlight::new(range, kind));
            } else if name == "fold" && fragment.contains('\n') {
                result.folds.push(range);
            } else if let Some(kind) = name.strip_prefix("symbol.") {
                result.symbols.push(Symbol {
                    name: fragment.split_whitespace().collect::<Vec<_>>().join(" "),
                    kind: kind.into(),
                    offset: range.start,
                });
            }
        }
        result.highlights = Highlight::flatten(result.highlights);
        result.folds.sort_by_key(|r| (r.start, r.end));
        result.folds.dedup();
        result.symbols.sort_by_key(|s| s.offset);
        result.symbols.dedup();
        result
    }
}

impl Highlight {
    /// Make an unmodified syntax span in the document's UTF-8 coordinates.
    pub fn new(range: ByteRange, kind: SyntaxKind) -> Self {
        Self {
            range,
            kind,
            modifiers: SyntaxModifiers::default(),
        }
    }

    /// Split nested ranges into disjoint runs. The innermost range wins, then the last equal-range capture.
    pub fn flatten(mut spans: Vec<Self>) -> Vec<Self> {
        spans.retain(|s| s.range.start < s.range.end);
        spans.sort_by_key(|s| s.range.start);
        let mut active = BinaryHeap::new();
        let mut result: Vec<Self> = Vec::new();
        let mut next = 0;
        let mut offset = 0;
        loop {
            while let Some(span) = spans.get(next).filter(|s| s.range.start <= offset) {
                active.push((span.range.start, Reverse(span.range.end), next));
                next += 1;
            }
            while active
                .peek()
                .is_some_and(|&(_, Reverse(end), _)| end <= offset)
            {
                active.pop();
            }
            let next_start = spans.get(next).map(|s| s.range.start);
            let Some(&(_, Reverse(end), index)) = active.peek() else {
                let Some(start) = next_start else { break };
                offset = start;
                continue;
            };
            let end = next_start.map_or(end, |start| end.min(start));
            let span = spans[index];
            if let Some(last) = result.last_mut().filter(|s| {
                s.range.end == offset && s.kind == span.kind && s.modifiers == span.modifiers
            }) {
                last.range.end = end;
            } else {
                result.push(Self {
                    range: ByteRange { start: offset, end },
                    ..span
                });
            }
            offset = end;
        }
        result
    }
}
