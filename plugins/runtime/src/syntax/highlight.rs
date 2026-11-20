//! Enrich presentation queries with the bundled grammar's captures and contextual token roles.

use super::{
    ByteRange, Capture, Instant, Language, MAX_CAPTURES, Node, Query, descendants, query_captures,
};
use nanobug_plugin_sdk::language::SyntaxKind::{self, *};
use std::collections::{BTreeMap, HashMap};

/// Reuse compiled built-in queries independently from untrusted plugin query strings.
#[derive(Default)]
pub(super) struct GrammarHighlights(HashMap<std::string::String, Query>);

impl GrammarHighlights {
    /// Replace lexical captures with finer roles, retaining folds and outline entries in document coordinates.
    pub(super) fn enrich(
        &mut self,
        name: &str,
        grammar: &Language,
        root: Node<'_>,
        text: &str,
        captures: &mut Vec<Capture>,
        deadline: Instant,
    ) -> bool {
        let mut roles = BTreeMap::<(usize, usize), (u8, SyntaxKind)>::new();
        let mut insert = |start, end, rank, kind| {
            if start < end {
                let slot = roles.entry((start, end)).or_insert((rank, kind));
                if rank >= slot.0 {
                    *slot = (rank, kind);
                }
            }
        };
        captures.retain(|cap| {
            let range = cap.range;
            if let Some(kind) = SyntaxKind::from_capture(&cap.name) {
                // Legacy categories invite refinement. A provider's specific role is authoritative.
                let rank = match kind {
                    Keyword | Type | Function | String | Constant | Comment | Attribute => 2,
                    _ => 10,
                };
                insert(range.start, range.end, rank, kind);
                false
            } else {
                if let Some(kind) = cap.name.strip_prefix("symbol.").and_then(symbol_kind) {
                    insert(range.start, range.end, 6, kind);
                }
                true
            }
        });
        if !self.0.contains_key(name) {
            let source = builtin_query(name);
            if let Ok(query) = Query::new(grammar, &source) {
                self.0.insert(name.into(), query);
            }
        }
        let mut complete = true;
        if let Some(query) = self.0.get(name) {
            let (standard, all) =
                query_captures(query, root, text.as_bytes(), 0, MAX_CAPTURES, deadline);
            complete &= all;
            for cap in standard {
                let r = cap.range;
                let Some(fragment) = text.get(r.start..r.end) else {
                    continue;
                };
                if let Some(kind) = grammar_role(name, &cap.name, fragment) {
                    let rank = match kind {
                        Variable => 0,
                        String | Type | Constant | Keyword | Function | Comment | Attribute => 1,
                        _ => 4,
                    };
                    insert(r.start, r.end, rank, kind);
                }
            }
        }
        let mut atomic = Vec::<std::ops::Range<usize>>::new();
        for node in descendants(root) {
            if Instant::now() >= deadline {
                complete = false;
                break;
            }
            let range = node.byte_range();
            let Some(value) = text.get(range.clone()) else {
                continue;
            };
            if matches!(
                node.kind(),
                "lifetime" | "label" | "regex" | "regex_literal"
            ) && atomic.last().is_none_or(|r| range.start >= r.end)
            {
                atomic.push(range.clone());
            }
            if let Some((rank, kind)) = contextual(name, node, value) {
                insert(range.start, range.end, rank, kind);
            }
            if matches!(
                node.kind(),
                "comment"
                    | "line_comment"
                    | "block_comment"
                    | "documentation_comment"
                    | "doc_comment"
            ) {
                for (offset, _) in value.match_indices(['T', 'F']) {
                    let marker = ["TODO", "FIXME"]
                        .into_iter()
                        .find(|m| value[offset..].starts_with(m));
                    if let Some(marker) = marker {
                        let end = offset + marker.len();
                        let part = |c: char| c.is_alphanumeric() || c == '_';
                        if !value[..offset].ends_with(part) && !value[end..].starts_with(part) {
                            insert(range.start + offset, range.start + end, 9, TodoComment);
                        }
                    }
                }
            }
        }
        // A grammar may also capture the apostrophe and identifier inside a lifetime or label.
        // The complete lexical token is the user-facing unit in these cases.
        roles.retain(|&(start, end), _| {
            let i = atomic.partition_point(|r| r.end <= start);
            !atomic.get(i).is_some_and(|r| {
                r.start <= start && end <= r.end && (r.start, r.end) != (start, end)
            })
        });
        let room = MAX_CAPTURES.saturating_sub(captures.len());
        complete &= roles.len() <= room;
        captures.extend(
            roles
                .into_iter()
                .take(room)
                .map(|((start, end), (_, kind))| Capture {
                    name: kind.capture_group()[1..].into(),
                    range: ByteRange { start, end },
                }),
        );
        captures.sort_by_key(|c| c.range.start);
        complete
    }
}

/// Query data is provided by the grammar versions already linked into the worker.
fn builtin_query(name: &str) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    let source = match name {
        "rust" => tree_sitter_rust::HIGHLIGHTS_QUERY,
        "javascript" => tree_sitter_javascript::HIGHLIGHT_QUERY,
        "javascriptreact" | "jsx" => {
            return Cow::Owned(
                [
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                ]
                .join("\n"),
            );
        }
        "typescript" | "tsx" => {
            let jsx = if name == "tsx" {
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            } else {
                ""
            };
            return Cow::Owned(
                [
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                    jsx,
                ]
                .join("\n"),
            );
        }
        "c" => tree_sitter_c::HIGHLIGHT_QUERY,
        "cpp" => {
            return Cow::Owned(
                [
                    tree_sitter_c::HIGHLIGHT_QUERY,
                    tree_sitter_cpp::HIGHLIGHT_QUERY,
                ]
                .join("\n"),
            );
        }
        "css" => tree_sitter_css::HIGHLIGHTS_QUERY,
        "html" | "vue" | "vue-template" => tree_sitter_html::HIGHLIGHTS_QUERY,
        "go" => tree_sitter_go::HIGHLIGHTS_QUERY,
        "java" => tree_sitter_java::HIGHLIGHTS_QUERY,
        "csharp" => tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
        "lua" => tree_sitter_lua::HIGHLIGHTS_QUERY,
        "php" => tree_sitter_php::HIGHLIGHTS_QUERY,
        "python" => tree_sitter_python::HIGHLIGHTS_QUERY,
        "ruby" => tree_sitter_ruby::HIGHLIGHTS_QUERY,
        "sql" => tree_sitter_sequel::HIGHLIGHTS_QUERY,
        "json" => tree_sitter_json::HIGHLIGHTS_QUERY,
        "toml" => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        "yaml" => tree_sitter_yaml::HIGHLIGHTS_QUERY,
        "svelte" => tree_sitter_svelte_ng::HIGHLIGHTS_QUERY,
        _ => "",
    };
    Cow::Borrowed(source)
}

fn symbol_kind(symbol: &str) -> Option<SyntaxKind> {
    Some(match symbol {
        "method" => Method,
        "class" => Class,
        "struct" | "union" => Struct,
        "trait" | "interface" => Interface,
        "enum" => Enum,
        "variant" => EnumMember,
        "mod" | "namespace" | "module" => Module,
        "macro" => Macro,
        "const" => Constant,
        "fn" | "function" => Function,
        "table" => SqlTable,
        _ => return None,
    })
}

/// Translate grammar naming conventions to editor roles before resolving overlapping captures.
fn grammar_role(language: &str, name: &str, value: &str) -> Option<SyntaxKind> {
    Some(match (language, name) {
        (_, "keyword") => keyword(value).unwrap_or(Keyword),
        ("json" | "yaml" | "toml", "property" | "string.special.key") => ConfigKey,
        ("css", "tag" | "attribute") => CssSelector,
        ("css", "property") => CssProperty,
        ("html" | "vue" | "vue-template" | "svelte" | "astro", "attribute") => TagAttribute,
        ("sql", "field") => SqlColumn,
        ("sql", "parameter") => SqlParameter,
        (_, "property" | "field" | "property.definition") => Field,
        (_, "escape" | "constant.character.escape") => Escape,
        (_, "string.regex" | "regex") => Regexp,
        (_, "function.call" | "function.definition") => Function,
        (_, "method" | "method.call" | "function.method.call") => Method,
        (_, "parameter") => Parameter,
        (_, "namespace") => Module,
        (_, "conditional") => Conditional,
        (_, "float") => Float,
        (_, "integer" | "constant.numeric") => Number,
        (_, "constant.builtin") => match value {
            "true" | "false" | "True" | "False" => Boolean,
            "null" | "nil" | "None" | "undefined" => Null,
            _ => Constant,
        },
        (_, "type.qualifier" | "storageclass" | "keyword.storage") => Modifier,
        (_, "keyword.operator") => Operator,
        (_, "tag.delimiter") => Bracket,
        _ => SyntaxKind::from_capture(name)
            .or_else(|| name.strip_prefix("keyword.").map(|_| Keyword))?,
    })
}

/// Keyword tokens are already identified by the grammar; identifiers with the same spelling are untouched.
fn keyword(word: &str) -> Option<SyntaxKind> {
    Some(match word {
        "if" | "else" | "elif" | "elsif" | "then" | "match" | "switch" | "case" | "when"
        | "unless" => Conditional,
        "for" | "while" | "do" | "loop" | "until" | "break" | "continue" | "foreach" => Repeat,
        "return" | "yield" => Return,
        "import" | "export" | "use" | "from" | "package" | "using" => Import,
        "try" | "catch" | "except" | "finally" | "raise" | "throw" | "throws" | "rescue" => {
            Exception
        }
        "pub" | "public" | "private" | "protected" | "static" | "readonly" | "mut" | "async"
        | "abstract" | "final" | "const" => Modifier,
        _ => return None,
    })
}

/// Classify grammar nodes whose semantic role depends on their parent, field or spelling.
fn contextual(language: &str, node: Node<'_>, value: &str) -> Option<(u8, SyntaxKind)> {
    let parent = node.parent()?;
    let p = parent.kind();
    let k = node.kind();
    let field = |name| parent.child_by_field_name(name) == Some(node);
    if matches!(
        p,
        "lifetime" | "label" | "escape_sequence" | "attribute_name"
    ) {
        return None;
    }
    if (language == "json" && p == "pair"
        || language == "yaml" && matches!(p, "block_mapping_pair" | "flow_pair"))
        && field("key")
    {
        return Some((8, ConfigKey));
    }
    let role = match k {
        "escape_sequence" | "escape_character" => Escape,
        "regex" | "regex_literal" | "regex_pattern" => Regexp,
        "char_literal" | "character_literal" | "character" => Character,
        "integer_literal"
        | "integer_value"
        | "integer"
        | "int_literal"
        | "decimal_integer_literal"
        | "hex_integer_literal"
        | "number_literal"
        | "number" => number_kind(value),
        "float" | "float_literal" | "floating_point_literal" | "float_value" => Float,
        "true" | "false" | "boolean" | "boolean_literal" | "boolean_scalar" | "keyword_true"
        | "keyword_false" => Boolean,
        "none" | "null" | "nil" | "null_literal" | "undefined" | "null_scalar" | "keyword_null" => {
            Null
        }
        "self" | "this" | "super" => BuiltinVariable,
        "lifetime" => Lifetime,
        "label" | "statement_identifier" => Label,
        "primitive_type" | "predefined_type" => BuiltinType,
        "tag_name" if language == "css" => CssSelector,
        "tag_name" => tag(value),
        "class_name" | "id_name" | "pseudo_class_selector" | "pseudo_element_selector"
            if language == "css" =>
        {
            CssSelector
        }
        "property_name" if language == "css" => CssProperty,
        "attribute_name"
            if matches!(
                language,
                "html" | "vue" | "vue-template" | "astro" | "svelte"
            ) =>
        {
            if value.starts_with("v-")
                || value.starts_with("x-")
                || value.starts_with([':', '@', '#', '[', '(', '*'])
                || ["bind-", "on-", "let-"]
                    .iter()
                    .any(|prefix| value.starts_with(prefix))
                || value.split_once(':').is_some_and(|(prefix, _)| {
                    matches!(
                        prefix,
                        "v-bind"
                            | "v-on"
                            | "bind"
                            | "on"
                            | "use"
                            | "let"
                            | "class"
                            | "style"
                            | "transition"
                            | "in"
                            | "out"
                            | "animate"
                            | "client"
                            | "set"
                            | "define"
                    )
                })
            {
                TemplateDirective
            } else {
                TagAttribute
            }
        }
        "bare_key" | "quoted_key" if language == "toml" => ConfigKey,
        "parameter" if language == "sql" => SqlParameter,
        "literal"
            if language == "sql"
                && value
                    .trim_start_matches(['+', '-'])
                    .starts_with(|c: char| c.is_ascii_digit()) =>
        {
            number_kind(value)
        }
        "line_comment" | "block_comment" | "comment"
            if value.starts_with("///")
                || value.starts_with("//!")
                || value.starts_with("/**")
                || value.starts_with("/*!") =>
        {
            DocComment
        }
        "identifier"
        | "field_identifier"
        | "property_identifier"
        | "type_identifier"
        | "shorthand_property_identifier"
        | "simple_identifier"
        | "name" => {
            return identifier_role(language, node, parent, value);
        }
        _ if !node.is_named()
            && matches!(
                p,
                "template_substitution" | "interpolation" | "interpolation_expression"
            )
            && matches!(value, "$" | "#" | "{" | "}") =>
        {
            Interpolation
        }
        _ if !node.is_named() && matches!(value, "${" | "#{") => Interpolation,
        _ if language == "rust" && p == "macro_invocation" && value == "!" => Macro,
        _ if !node.is_named()
            && value.starts_with('#')
            && value.len() > 1
            && matches!(language, "c" | "cpp") =>
        {
            Directive
        }
        _ if !node.is_named() && matches!(value, "(" | ")" | "[" | "]" | "{" | "}") => Bracket,
        _ if !node.is_named() && matches!(value, "," | ";" | ":" | "." | "::") => Delimiter,
        _ if !node.is_named()
            && !value.is_empty()
            && value.len() <= 4
            && value.bytes().all(|b| b"+-*/%=!&|^<>?~".contains(&b)) =>
        {
            return Some((3, Operator));
        }
        _ if !node.is_named() => keyword(value)?,
        _ => return None,
    };
    Some((8, role))
}

/// Exponent letters inside a hexadecimal integer are digits.
fn number_kind(value: &str) -> SyntaxKind {
    let unsigned = value.trim_start_matches(['+', '-']);
    let exponent = if unsigned.starts_with("0x") || unsigned.starts_with("0X") {
        ['p', 'P']
    } else {
        ['e', 'E']
    };
    if unsigned.contains('.') || unsigned.contains(exponent) {
        Float
    } else {
        Number
    }
}

/// Resolve identifiers by their grammar fields and declaration scope.
fn identifier_role(
    language: &str,
    node: Node<'_>,
    parent: Node<'_>,
    value: &str,
) -> Option<(u8, SyntaxKind)> {
    let p = parent.kind();
    let k = node.kind();
    let field = |name| parent.child_by_field_name(name) == Some(node);
    let role = if language == "sql" {
        if field("alias") {
            SqlAlias
        } else if p == "field" || p == "column_definition" {
            SqlColumn
        } else if p == "object_reference"
            && parent.parent().is_some_and(|n| n.kind() != "invocation")
        {
            SqlTable
        } else {
            return Some((0, Variable));
        }
    } else if matches!(
        p,
        "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element"
    ) && field("name")
    {
        tag(value)
    } else if p == "jsx_attribute" {
        TagAttribute
    } else if p == "type_parameter" {
        TypeParameter
    } else if p == "enum_variant"
        || p == "enum_member_declaration"
        || p == "enum_assignment" && field("name")
        || p == "enum_body"
    {
        EnumMember
    } else if matches!(
        p,
        "required_parameter"
            | "optional_parameter"
            | "formal_parameter"
            | "parameter_declaration"
            | "parameter"
            | "typed_parameter"
            | "parameters"
            | "formal_parameters"
            | "lambda_parameters"
            | "closure_parameters"
    ) && !field("type")
        && !field("value")
        && k != "type_identifier"
    {
        Parameter
    } else if field("name")
        && matches!(
            p,
            "constructor_declaration" | "primary_constructor" | "secondary_constructor"
        )
    {
        Constructor
    } else if field("name")
        && (matches!(
            p,
            "method_definition" | "method_declaration" | "method_signature"
        ) || is_member_definition(language, parent))
    {
        if matches!(
            (language, value),
            ("javascript" | "jsx" | "typescript" | "tsx", "constructor")
                | ("python", "__init__")
                | ("ruby", "initialize")
        ) {
            Constructor
        } else {
            Method
        }
    } else if matches!(p, "new_expression" | "object_creation_expression")
        && (field("constructor") || field("type"))
    {
        Constructor
    } else if p == "macro_invocation" && field("macro") {
        Macro
    } else if language == "rust" && p == "attribute" {
        Attribute
    } else if p == "mod_item" || p == "namespace_declaration" && field("name") {
        Module
    } else if p == "class_declaration" && field("name") {
        Class
    } else if p == "struct_item" && field("name") {
        Struct
    } else if matches!(p, "interface_declaration" | "trait_item") && field("name") {
        Interface
    } else if matches!(p, "enum_item" | "enum_declaration") && field("name") {
        Enum
    } else if matches!(
        p,
        "field_expression" | "member_expression" | "selector_expression" | "attribute"
    ) && (field("field") || field("property") || field("attribute"))
    {
        if parent
            .parent()
            .is_some_and(|n| n.child_by_field_name("function") == Some(parent))
        {
            Method
        } else {
            Field
        }
    } else if matches!(k, "field_identifier" | "property_identifier") {
        return Some((3, Field));
    } else if k == "type_identifier" {
        return Some((1, Type));
    } else {
        return Some((0, Variable));
    };
    Some((8, role))
}

/// Classify functions declared directly in an implementation, class or trait body.
fn is_member_definition(language: &str, declaration: Node<'_>) -> bool {
    let enclosing = declaration
        .parent()
        .and_then(|body| body.parent())
        .map(|scope| scope.kind());
    match (language, declaration.kind()) {
        ("rust", "function_item" | "function_signature_item") => {
            matches!(enclosing, Some("impl_item" | "trait_item"))
        }
        ("python", "function_definition") => {
            matches!(enclosing, Some("class_definition"))
                || declaration.parent().is_some_and(|n| {
                    n.kind() == "decorated_definition"
                        && n.parent()
                            .and_then(|b| b.parent())
                            .is_some_and(|scope| scope.kind() == "class_definition")
                })
        }
        ("ruby", "method" | "singleton_method") => true,
        _ => false,
    }
}

fn tag(value: &str) -> SyntaxKind {
    if value.starts_with(char::is_uppercase) || value.contains('-') || value.starts_with("svelte:")
    {
        Component
    } else {
        Tag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each bundled query must compile against its actual grammar before it can color documents.
    #[test]
    fn builtin_highlight_queries_match_their_grammars() {
        for name in [
            "rust",
            "javascript",
            "jsx",
            "typescript",
            "tsx",
            "c",
            "cpp",
            "css",
            "html",
            "vue",
            "go",
            "java",
            "csharp",
            "lua",
            "php",
            "python",
            "ruby",
            "sql",
            "json",
            "toml",
            "yaml",
            "svelte",
        ] {
            let lang = super::super::grammar(name).unwrap();
            Query::new(&lang, &builtin_query(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
