//! Shared, bounded Tree-sitter parsing in its own worker role.

mod highlight;
use crate::Output;
use nanobug_plugin_sdk::language::HIGHLIGHT_QUERY_PREFIX;
use nanobug_plugin_sdk::{
    Diagnostic, DiagnosticSeverity, DocumentSnapshot, ErrorKind, Reply, Request,
    language::{ByteRange, Capture, SyntaxQuery},
    receive,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io::BufRead,
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{
    Language, Node, Parser, Query, QueryCursor, QueryCursorOptions, StreamingIterator, Tree,
};

const MAX_NODES: usize = 2048;
/// Keep complete native node lists under the bounded reply size.
const MAX_SELECTION_NODES: usize = 24_000;
/// Cap both capture traffic and syntax error rows for a single snapshot.
const MAX_CAPTURES: usize = 6144;
const MAX_DIAGNOSTICS: usize = 80;
const MAX_EMBEDDED_RANGES: usize = 32;
const MAX_CACHED: usize = 12;
const PARSE_BUDGET: Duration = Duration::from_millis(500);
type Key = (String, u64, u64);

/// A view of the same versioned Tree-sitter tree.
enum Projection<'a> {
    /// Legacy node details for plugins.
    Outline,
    /// Named captures and error diagnostics for a plugin query.
    Query(&'a str),
    /// Named UTF-8 byte intervals for native selection.
    Selection,
}

/// Reuse parse results for the same document version and grammar.
pub(super) fn serve(mut input: impl BufRead, output: Output) -> anyhow::Result<()> {
    let mut syntax = Syntax::default();
    while let Some(request) = receive(&mut input)? {
        let (id, result) = match request {
            Request::Parse {
                id,
                language,
                document,
            } => (id, syntax.run(language, &document, Projection::Outline)),
            Request::QuerySyntax {
                id,
                language,
                query,
                document,
            } => (
                id,
                syntax.run(language, &document, Projection::Query(&query)),
            ),
            Request::SyntaxNodes {
                id,
                language,
                document,
            } => (id, syntax.run(language, &document, Projection::Selection)),
            _ => anyhow::bail!("unexpected syntax request"),
        };
        match result {
            Ok(value) => output.send(Reply::Parsed { id, value })?,
            Err(error) => output.error(id, ErrorKind::Invalid, error)?,
        }
    }
    Ok(())
}

/// Keep trees independently of the requested projection so plugins share the same parse.
#[derive(Default)]
struct Syntax {
    parser: Parser,
    trees: VecDeque<(Key, Tree, Option<Value>)>,
    queries: VecDeque<(String, String, Query)>,
    highlights: highlight::GrammarHighlights,
    embedded_queries: BTreeMap<&'static str, Query>,
}

impl Syntax {
    /// Parse or reuse the tree for `doc` in `language` and produce its requested `projection`.
    fn run(
        &mut self,
        language: String,
        doc: &DocumentSnapshot,
        projection: Projection<'_>,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            doc.text.len() <= 120 * 1024,
            "Document exceeds syntax worker limit"
        );
        let base = grammar(&language).ok_or_else(|| anyhow::anyhow!("Grammar is not available"))?;
        let deadline = Instant::now() + PARSE_BUDGET;
        let key = (language, doc.opening, doc.revision);
        let mut entry = if let Some(index) = self.trees.iter().position(|entry| entry.0 == key) {
            self.trees.remove(index).expect("cached syntax tree")
        } else {
            let tree = parse_tree(&mut self.parser, &base, doc.text.as_bytes(), deadline)?;
            if self.trees.len() == MAX_CACHED {
                self.trees.pop_front();
            }
            (key, tree, None)
        };
        let result = match projection {
            Projection::Query(source) => self.query(
                &entry.0.0,
                &base,
                source,
                entry.1.root_node(),
                doc,
                deadline,
            ),
            Projection::Selection => selection_nodes(
                &mut self.parser,
                &entry.0.0,
                &doc.text,
                entry.1.root_node(),
                deadline,
            )
            .and_then(|nodes| Ok(serde_json::to_value(nodes)?)),
            Projection::Outline => {
                if let Some(value) = &entry.2 {
                    let mut value = value.clone();
                    value["cached"] = Value::Bool(true);
                    Ok(value)
                } else {
                    project_nodes(
                        &mut self.parser,
                        &entry.0.0,
                        doc,
                        entry.1.root_node(),
                        deadline,
                    )
                    .inspect(|value| entry.2 = Some(value.clone()))
                }
            }
        };
        self.trees.push_back(entry);
        result
    }

    /// Compile and reuse a small grammar query, then return captures and parser errors for `doc`.
    fn query(
        &mut self,
        language: &str,
        grammar: &Language,
        source: &str,
        root: Node<'_>,
        doc: &DocumentSnapshot,
        deadline: Instant,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(source.len() <= 8 * 1024, "Syntax query exceeds size limit");
        let entry = if let Some(index) = self
            .queries
            .iter()
            .position(|entry| entry.0 == language && entry.1 == source)
        {
            self.queries.remove(index).expect("cached syntax query")
        } else {
            let query = Query::new(grammar, source)?;
            if self.queries.len() == MAX_CACHED {
                self.queries.pop_front();
            }
            (language.into(), source.into(), query)
        };
        let result = (|| {
            let mut result = capture(&entry.2, root, doc, deadline)?;
            let rich = source.starts_with(HIGHLIGHT_QUERY_PREFIX);
            if rich {
                result.complete &= self.highlights.enrich(
                    language,
                    grammar,
                    root,
                    &doc.text,
                    &mut result.captures,
                    deadline,
                );
            }
            if (0..entry.2.pattern_count()).any(|index| {
                entry
                    .2
                    .property_settings(index)
                    .iter()
                    .any(|setting| setting.key.as_ref() == "nanobug.embedded")
            }) {
                self.append_embedded(language, root, &doc.text, &mut result, deadline, rich);
            }
            Ok(serde_json::to_value(result)?)
        })();
        self.queries.push_back(entry);
        result
    }

    /// Add captures from inline scripts, styles and Astro frontmatter in document coordinates.
    fn append_embedded(
        &mut self,
        language: &str,
        root: Node<'_>,
        text: &str,
        result: &mut SyntaxQuery,
        deadline: Instant,
        rich: bool,
    ) {
        if !matches!(
            language,
            "html" | "vue" | "vue-template" | "svelte" | "astro"
        ) {
            return;
        }
        let vue = matches!(language, "vue" | "vue-template");
        let mut regions = descendants(root)
            .take_while(|_| Instant::now() < deadline)
            .filter(|node| !vue || node.parent() == Some(root))
            .filter_map(|node| {
                if language == "astro" && node.kind() == "frontmatter" {
                    node.named_child(0)
                        .filter(|child| child.kind() == "frontmatter_js_block")
                        .map(|child| ("typescript", child.byte_range()))
                } else {
                    embedded_region(node, text, false)
                }
            });
        for (grammar_name, range) in regions.by_ref().take(MAX_EMBEDDED_RANGES) {
            let limit = MAX_CAPTURES - result.captures.len();
            if limit == 0 {
                break;
            }
            let Some(grammar) = grammar(grammar_name) else {
                continue;
            };
            if let std::collections::btree_map::Entry::Vacant(slot) =
                self.embedded_queries.entry(grammar_name)
            {
                let Ok(query) = Query::new(&grammar, &embedded_highlights(grammar_name)) else {
                    result.complete = false;
                    continue;
                };
                slot.insert(query);
            }
            let bytes = &text.as_bytes()[range.clone()];
            let Ok(tree) = parse_tree(&mut self.parser, &grammar, bytes, deadline) else {
                result.complete = false;
                break;
            };
            let (mut captures, complete) = query_captures(
                &self.embedded_queries[grammar_name],
                tree.root_node(),
                bytes,
                0,
                limit,
                deadline,
            );
            if rich {
                result.complete &= self.highlights.enrich(
                    grammar_name,
                    &grammar,
                    tree.root_node(),
                    &text[range.clone()],
                    &mut captures,
                    deadline,
                );
            }
            result.complete &= complete && captures.len() <= limit;
            captures.truncate(limit);
            for capture in &mut captures {
                capture.range.start += range.start;
                capture.range.end += range.start;
            }
            result.captures.extend(captures);
        }
        result.complete &= result.captures.len() < MAX_CAPTURES
            && regions.next().is_none()
            && Instant::now() < deadline;
        result.captures.sort_by_key(|capture| capture.range.start);
    }
}

/// Collect named ranges, including the inline grammars used in Markdown and web documents.
fn selection_nodes(
    parser: &mut Parser,
    language: &str,
    text: &str,
    root: Node<'_>,
    deadline: Instant,
) -> anyhow::Result<Vec<ByteRange>> {
    let mut ranges = Vec::new();
    append_selection_nodes(&mut ranges, root, 0, deadline)?;
    let vue = matches!(language, "vue" | "vue-template");
    if matches!(
        language,
        "markdown" | "html" | "vue" | "vue-template" | "astro" | "svelte"
    ) {
        let regions = descendants(root).filter_map(|node| {
            if language == "markdown" {
                (node.kind() == "inline").then(|| ("markdown_inline", node.byte_range()))
            } else if language == "astro" && node.kind() == "frontmatter" {
                node.named_child(0)
                    .filter(|child| child.kind() == "frontmatter_js_block")
                    .map(|child| ("typescript", child.byte_range()))
            } else {
                (!vue || node.parent() == Some(root))
                    .then(|| embedded_region(node, text, vue))
                    .flatten()
            }
        });
        for (language, region) in regions {
            let grammar = grammar(language).expect("built-in embedded selection grammar");
            let tree = parse_tree(parser, &grammar, &text.as_bytes()[region.clone()], deadline)?;
            append_selection_nodes(&mut ranges, tree.root_node(), region.start, deadline)?;
        }
    }
    Ok(ranges)
}

/// Add nonempty named nodes below `root`, translating them by `offset` before `deadline`.
fn append_selection_nodes(
    ranges: &mut Vec<ByteRange>,
    root: Node<'_>,
    offset: usize,
    deadline: Instant,
) -> anyhow::Result<()> {
    for node in descendants(root).filter(Node::is_named) {
        anyhow::ensure!(Instant::now() < deadline, "Syntax selection timed out");
        let start = offset + node.start_byte();
        let end = offset + node.end_byte();
        if start < end {
            anyhow::ensure!(ranges.len() < MAX_SELECTION_NODES, "Too many syntax nodes");
            ranges.push(ByteRange { start, end });
        }
    }
    Ok(())
}

/// Return query captures and parser recovery errors in UTF-8 byte coordinates.
fn capture(
    query: &Query,
    root: Node<'_>,
    doc: &DocumentSnapshot,
    deadline: Instant,
) -> anyhow::Result<SyntaxQuery> {
    let (captures, complete) =
        query_captures(query, root, doc.text.as_bytes(), 0, MAX_CAPTURES, deadline);
    anyhow::ensure!(Instant::now() < deadline, "Syntax query timed out");
    let diagnostics = descendants(root)
        .take_while(|_| Instant::now() < deadline)
        .filter(|node| node.is_error() || node.is_missing())
        .take(MAX_DIAGNOSTICS)
        .map(|node| Diagnostic {
            start: node.start_byte(),
            end: node.end_byte(),
            severity: DiagnosticSeverity::Error,
            message: if node.is_missing() {
                format!("Expected `{}`", node.kind())
            } else {
                "Unexpected syntax".into()
            },
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(Instant::now() < deadline, "Syntax diagnostics timed out");
    Ok(SyntaxQuery {
        opening: doc.opening,
        revision: doc.revision,
        complete: complete && diagnostics.len() < MAX_DIAGNOSTICS,
        captures,
        diagnostics,
    })
}

/// Collect at most `limit` captures within the shared deadline, offset into the original text.
fn query_captures(
    query: &Query,
    root: Node<'_>,
    bytes: &[u8],
    offset: usize,
    limit: usize,
    deadline: Instant,
) -> (Vec<Capture>, bool) {
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(128);
    let mut progress = |_: &tree_sitter::QueryCursorState| {
        if Instant::now() < deadline {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    };
    let options = QueryCursorOptions::new().progress_callback(&mut progress);
    let mut matches = cursor.captures_with_options(query, root, bytes, options);
    let mut captures = Vec::new();
    while captures.len() < limit {
        let Some((matched, index)) = matches.next() else {
            break;
        };
        let item = matched.captures[*index];
        captures.push(Capture {
            name: query.capture_names()[item.index as usize].into(),
            range: ByteRange {
                start: offset + item.node.start_byte(),
                end: offset + item.node.end_byte(),
            },
        });
    }
    drop(matches);
    let complete =
        captures.len() < limit && !cursor.did_exceed_match_limit() && Instant::now() < deadline;
    (captures, complete)
}

/// Keep the original node-list response for consumers of ParseDocument.
fn project_nodes(
    parser: &mut Parser,
    language: &str,
    doc: &DocumentSnapshot,
    root: Node<'_>,
    deadline: Instant,
) -> anyhow::Result<Value> {
    let (nodes, complete) = collect_nodes(root, 0, MAX_NODES);
    let mut result = json!({
        "opening": doc.opening, "revision": doc.revision, "cached": false,
        "complete": complete, "has_error": root.has_error(), "nodes": nodes
    });
    let vue = matches!(language, "vue" | "vue-template");
    if language == "html" || vue {
        let (embedded, complete) = parse_embedded(parser, root, &doc.text, vue, deadline)?;
        result["embedded"] = Value::Array(embedded);
        result["embedded_complete"] = Value::Bool(complete);
    }
    Ok(result)
}

/// A combined request shares one time budget across its host and embedded grammars.
fn parse_tree(
    parser: &mut Parser,
    language: &Language,
    bytes: &[u8],
    deadline: Instant,
) -> anyhow::Result<Tree> {
    parser.set_language(language)?;
    anyhow::ensure!(Instant::now() < deadline, "Syntax parse timed out");
    let mut budget = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut budget);
    let tree = parser.parse_with_options(
        &mut |offset, _| &bytes[offset.min(bytes.len())..],
        None,
        Some(options),
    );
    let Some(tree) = tree else {
        parser.reset();
        anyhow::bail!("Syntax parse timed out");
    };
    anyhow::ensure!(Instant::now() < deadline, "Syntax parse timed out");
    Ok(tree)
}

/// Return nodes in source order without building a second stack of their children.
fn descendants(root: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = root.walk();
    let mut next = Some(root);
    std::iter::from_fn(move || {
        let node = next.take()?;
        if cursor.goto_first_child() {
            next = Some(cursor.node());
        } else {
            loop {
                if cursor.goto_next_sibling() {
                    next = Some(cursor.node());
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
            }
        }
        Some(node)
    })
}

fn collect_nodes(root: Node<'_>, offset: usize, limit: usize) -> (Vec<Value>, bool) {
    let mut named = descendants(root).filter(Node::is_named);
    let nodes = named
        .by_ref()
        .take(limit)
        .map(|node| {
            json!({
                "kind": node.kind(), "start": offset + node.start_byte(),
                "end": offset + node.end_byte(), "error": node.is_error()
            })
        })
        .collect();
    (nodes, named.next().is_none())
}

/// Parse closed HTML regions without reading referenced files or executing their content.
fn parse_embedded(
    parser: &mut Parser,
    root: Node<'_>,
    text: &str,
    vue: bool,
    deadline: Instant,
) -> anyhow::Result<(Vec<Value>, bool)> {
    let mut regions = descendants(root)
        .filter(|node| !vue || node.parent() == Some(root))
        .filter_map(|node| embedded_region(node, text, vue));
    let mut remaining = MAX_NODES;
    let mut parsed = Vec::new();
    let mut all_complete = true;
    for (language, range) in regions.by_ref().take(MAX_EMBEDDED_RANGES) {
        let grammar = grammar(language).expect("built-in embedded grammar");
        let tree = parse_tree(parser, &grammar, &text.as_bytes()[range.clone()], deadline)?;
        let root = tree.root_node();
        let (nodes, complete) = collect_nodes(root, range.start, remaining);
        remaining -= nodes.len();
        all_complete &= complete;
        parsed.push(json!({
            "language": language, "start": range.start, "end": range.end,
            "complete": complete, "has_error": root.has_error(), "nodes": nodes
        }));
    }
    Ok((parsed, all_complete && regions.next().is_none()))
}

fn embedded_region(node: Node<'_>, text: &str, vue: bool) -> Option<(&'static str, Range<usize>)> {
    let kind = node.kind();
    if !(matches!(kind, "script_element" | "style_element") || vue && kind == "element") {
        return None;
    }
    let start = node
        .named_child(0)
        .filter(|tag| tag.kind() == "start_tag")?;
    let last = u32::try_from(node.named_child_count().checked_sub(1)?).ok()?;
    let end = node
        .named_child(last)
        .filter(|tag| tag.kind() == "end_tag" && !tag.is_missing())?;
    if attribute(start, text, "src").is_some() {
        return None;
    }
    let lang = attribute(start, text, "lang")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let language = match kind {
        "script_element" => script_language(&lang, attribute(start, text, "type"))?,
        "style_element" if matches!(lang.as_str(), "" | "css") => {
            let style_type = attribute(start, text, "type").unwrap_or_default();
            if !style_type.is_empty() && !style_type.eq_ignore_ascii_case("text/css") {
                return None;
            }
            "css"
        }
        "element" if matches!(lang.as_str(), "" | "html") => {
            let name = start.named_child(0)?;
            if !text
                .get(name.byte_range())?
                .eq_ignore_ascii_case("template")
            {
                return None;
            }
            "html"
        }
        _ => return None,
    };
    (start.end_byte() <= end.start_byte()).then_some((language, start.end_byte()..end.start_byte()))
}

fn attribute<'text>(tag: Node<'_>, text: &'text str, name: &str) -> Option<&'text str> {
    let mut cursor = tag.walk();
    for node in tag
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "attribute")
    {
        let key = node.named_child(0)?;
        if text.get(key.byte_range())?.eq_ignore_ascii_case(name) {
            let value = node
                .named_child(1)
                .and_then(|value| text.get(value.byte_range()));
            return Some(value.unwrap_or_default().trim_matches(['\'', '"']).trim());
        }
    }
    None
}

fn script_language(lang: &str, content_type: Option<&str>) -> Option<&'static str> {
    if !lang.is_empty() {
        return match lang {
            "js" | "javascript" | "jsx" => Some("javascript"),
            "ts" | "typescript" => Some("typescript"),
            "tsx" => Some("tsx"),
            _ => None,
        };
    }
    let content_type = content_type.unwrap_or_default().split(';').next()?.trim();
    match content_type.to_ascii_lowercase().as_str() {
        ""
        | "module"
        | "text/javascript"
        | "application/javascript"
        | "text/ecmascript"
        | "application/ecmascript" => Some("javascript"),
        "application/json" | "application/ld+json" | "importmap" | "speculationrules" => {
            Some("json")
        }
        _ => None,
    }
}

/// Resolve worker-owned captures for a detected inline language.
fn embedded_highlights(name: &str) -> String {
    match name {
        "javascript" => SCRIPT_HIGHLIGHTS.to_owned(),
        "typescript" => [SCRIPT_HIGHLIGHTS, TYPESCRIPT_HIGHLIGHTS].join("\n"),
        "tsx" => [SCRIPT_HIGHLIGHTS, TYPESCRIPT_HIGHLIGHTS, JSX_HIGHLIGHTS].join("\n"),
        "css" => STYLE_HIGHLIGHTS.to_owned(),
        "json" => JSON_HIGHLIGHTS.to_owned(),
        _ => String::new(),
    }
}

/// Syntax shared by the ECMAScript-family embedded grammars.
const SCRIPT_HIGHLIGHTS: &str = r#"
[
  "as" "async" "await" "break" "case" "catch" "class" "const" "continue"
  "debugger" "default" "delete" "do" "else" "export" "extends" "finally"
  "for" "from" "function" "if" "import" "in" "instanceof" "let" "new" "of"
  "return" "static" "switch" "throw" "try" "typeof" "var" "void" "while" "yield"
] @keyword

[(comment) (hash_bang_line)] @comment
[(string) (template_string) (regex)] @string
[(number) (true) (false) (null) (undefined)] @constant

(function_declaration name: (identifier) @function @symbol.function)
(generator_function_declaration name: (identifier) @function @symbol.function)
(method_definition name: (property_identifier) @function @symbol.method)
(variable_declarator
  name: (identifier) @function @symbol.function
  value: [(arrow_function) (function_expression)])
(class_declaration name: (_) @type @symbol.class)
(call_expression function: (identifier) @function)
(call_expression function: (member_expression property: (property_identifier) @function))
(new_expression constructor: (identifier) @type)

[(statement_block) (class_body) (object) (array)] @fold
"#;

/// Additional types and declarations supported by TypeScript and TSX scripts.
const TYPESCRIPT_HIGHLIGHTS: &str = r#"
[
  "abstract" "declare" "enum" "implements" "interface" "keyof" "namespace"
  "private" "protected" "public" "type" "readonly" "override" "satisfies"
] @keyword
[(type_identifier) (predefined_type)] @type
(decorator (identifier) @attribute)
(interface_declaration name: (type_identifier) @symbol.interface)
(type_alias_declaration name: (type_identifier) @symbol.type)
(enum_declaration name: (identifier) @type @symbol.enum)
(abstract_class_declaration name: (type_identifier) @symbol.class)
(function_signature name: (identifier) @function @symbol.function)
(method_signature name: (property_identifier) @function @symbol.method)
[(interface_body) (enum_body) (object_type)] @fold
"#;

/// Distinguish JSX tags and attributes in embedded TSX.
const JSX_HIGHLIGHTS: &str = r#"
(jsx_opening_element) @fold
(jsx_element) @fold
(jsx_self_closing_element) @fold
(jsx_opening_element name: (identifier) @type)
(jsx_closing_element name: (identifier) @type)
(jsx_self_closing_element name: (identifier) @type)
(jsx_attribute (property_identifier) @attribute)
"#;

/// CSS declaration, selector and folding captures for inline styles.
const STYLE_HIGHLIGHTS: &str = r#"
[(tag_name) (unit)] @type
[(attribute_name) (property_name) (class_name) (id_name) (feature_name)] @attribute
(function_name) @function
(string_value) @string
[(integer_value) (float_value) (color_value)] @constant
[(comment) (js_comment)] @comment

[
  "@charset" "@import" "@media" "@keyframes" "@namespace" "@supports"
  (at_keyword) (important) (from) (to)
] @keyword

(rule_set (selectors) @symbol.selector)
(keyframes_statement (keyframes_name) @symbol.keyframes)

[(block) (keyframe_block_list) (comment)] @fold
"#;

/// JSON values and collection folds in data-script tags.
const JSON_HIGHLIGHTS: &str = r#"
(string) @string
[(number) (null) (true) (false)] @constant
(comment) @comment
[(array) (object)] @fold
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the same opt-in query and SDK projection used by compiled language plugins.
    #[test]
    fn provider_can_assign_a_specific_role_to_framework_syntax() {
        use nanobug_plugin_sdk::language::{DocumentFeatures, SyntaxKind};
        let query = format!("{HIGHLIGHT_QUERY_PREFIX}(property_identifier) @function.builtin");
        let doc = DocumentSnapshot {
            opening: 9,
            revision: 1,
            path: None,
            text: "app.special()".into(),
        };
        let result = Syntax::default()
            .run("javascript".into(), &doc, Projection::Query(&query))
            .unwrap();
        let query: SyntaxQuery = serde_json::from_value(result).unwrap();
        let spans = DocumentFeatures::from_captures(&doc.text, query.captures).highlights;
        assert!(
            spans
                .iter()
                .any(|s| &doc.text[s.range.start..s.range.end] == "special"
                    && s.kind == SyntaxKind::BuiltinFunction)
        );
    }

    /// Verify representative tokens and their innermost style in each language family.
    #[test]
    fn rich_roles_reach_source_tokens_in_each_family() {
        use nanobug_plugin_sdk::language::{DocumentFeatures, SyntaxKind::*};
        let mut worker = Syntax::default();
        let cases: &[(
            &str,
            &str,
            &[(&str, nanobug_plugin_sdk::language::SyntaxKind)],
        )] = &[
            (
                "rust",
                r#"/// TODO explain
fn save<'a,T>(limit: usize) { let user = true; user.save(); user.name; println!("中\n"); 'outer: loop { break 'outer; } }"#,
                &[
                    ("TODO", TodoComment),
                    ("fn save", Keyword),
                    ("'a", Lifetime),
                    ("T>", TypeParameter),
                    ("limit", Parameter),
                    ("usize", BuiltinType),
                    ("user =", Variable),
                    ("true", Boolean),
                    ("save();", Method),
                    ("name;", Field),
                    ("println", Macro),
                    ("\\n", Escape),
                    ("'outer:", Label),
                ],
            ),
            (
                "rust",
                "struct Buf; impl Buf { fn reset(&mut self) {} }",
                &[("reset", Method)],
            ),
            (
                "python",
                "class User:\n    def __init__(self): pass\n    def save(self): pass\n",
                &[("__init__", Constructor), ("save", Method)],
            ),
            (
                "html",
                r#"<svg xml:lang="en"></svg>"#,
                &[("xml:lang", TagAttribute)],
            ),
            (
                "javascript",
                "const value = `prefix ${name}`; class Page { constructor() {} }",
                &[
                    ("${", Interpolation),
                    ("name", Variable),
                    ("constructor", Constructor),
                ],
            ),
            (
                "typescript",
                "class User { save(limit: number) { return limit + 42; } }",
                &[
                    ("User", Class),
                    ("save", Method),
                    ("limit:", Parameter),
                    ("return", Return),
                    ("+", Operator),
                    ("42", Number),
                ],
            ),
            (
                "tsx",
                "const v = <UserCard disabled />;",
                &[("UserCard", Component), ("disabled", TagAttribute)],
            ),
            (
                "vue",
                "<UserCard v-if=\"ok\" disabled></UserCard>",
                &[
                    ("UserCard", Component),
                    ("v-if", TemplateDirective),
                    ("disabled", TagAttribute),
                ],
            ),
            (
                "css",
                ".card { color: red; }",
                &[("card", CssSelector), ("color", CssProperty)],
            ),
            (
                "sql",
                "SELECT u.name AS title FROM users AS u WHERE id = $1;",
                &[
                    ("name", SqlColumn),
                    ("title", SqlAlias),
                    ("users", SqlTable),
                    ("$1", SqlParameter),
                ],
            ),
            (
                "json",
                r#"{"name":"name","v":null}"#,
                &[(r#""name":"#, ConfigKey), ("null", Null)],
            ),
            ("toml", "port = 42", &[("port", ConfigKey), ("42", Number)]),
            (
                "yaml",
                "port: true",
                &[("port", ConfigKey), ("true", Boolean)],
            ),
            (
                "c",
                "#include <stdlib.h>\nint main(void) { return 0; }",
                &[("#include", Directive), ("return", Return)],
            ),
        ];
        let mut errors = Vec::new();
        for (i, &(language, text, expected)) in cases.iter().enumerate() {
            let doc = DocumentSnapshot {
                opening: i as u64,
                revision: 0,
                path: None,
                text: text.into(),
            };
            let result = worker
                .run(
                    language.into(),
                    &doc,
                    Projection::Query(HIGHLIGHT_QUERY_PREFIX),
                )
                .unwrap();
            let result: SyntaxQuery = serde_json::from_value(result).unwrap();
            let features = DocumentFeatures::from_captures(text, result.captures);
            for &(fragment, kind) in expected {
                let offset = text.find(fragment).unwrap();
                let actual = features
                    .highlights
                    .iter()
                    .find(|s| s.range.start <= offset && offset < s.range.end)
                    .map(|s| s.kind);
                if actual != Some(kind) {
                    errors.push(format!(
                        "{language} {fragment:?}: {actual:?}, expected {kind:?}"
                    ));
                }
            }
        }
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    /// Return named ranges in original UTF-8 coordinates, including Markdown inline nodes.
    #[test]
    fn selection_uses_named_tree_sitter_nodes() {
        let mut syntax = Syntax::default();
        let cases = [
            (
                "rust",
                "fn run() { println!(\"中文\"); }",
                &["run", "\"中文\"", "中文"][..],
                "{",
            ),
            (
                "markdown",
                "# 标题\n\nSee **强调** and [中文](https://example.test).\n",
                &["**强调**", "中文", "https://example.test"][..],
                "**",
            ),
        ];
        for (language, text, expected, anonymous) in cases {
            let doc = DocumentSnapshot {
                path: None,
                opening: 0,
                revision: 0,
                text: text.into(),
            };
            let value = syntax
                .run(language.into(), &doc, Projection::Selection)
                .unwrap();
            let ranges: Vec<ByteRange> = serde_json::from_value(value).unwrap();
            let selected: Vec<&str> = ranges
                .iter()
                .map(|range| &text[range.start..range.end])
                .collect();
            for expected in expected {
                assert!(selected.contains(expected), "{language}: {selected:?}");
            }
            assert!(!selected.contains(&anonymous), "{language}: {selected:?}");
        }
    }

    /// The same outer query can opt in to embedded captures without changing diagnostics.
    #[test]
    fn markup_queries_include_inline_captures_only_when_requested() {
        let mut syntax = Syntax::default();
        let plain_query = "(tag_name) @type";
        let opt_in_query = "((document) (#set! nanobug.embedded)) (tag_name) @type";
        for (index, (language, text, expected)) in [
            (
                "html",
                "<p>世界</p><script>function hello() {}</script><style>p {color: red;}</style>",
                &[("function", "hello"), ("attribute", "color")][..],
            ),
            (
                "vue",
                "<template><p>世界</p></template><script setup lang=\"ts\">let value: number;</script>",
                &[("keyword", "let"), ("type", "number")][..],
            ),
            (
                "svelte",
                "<p>世界</p><script lang=\"ts\">let value: number;</script><style>p {color: red;}</style>",
                &[("type", "number"), ("attribute", "color")][..],
            ),
            (
                "astro",
                "---\nimport Card from './Card.astro';\n---\n<Card />",
                &[("keyword", "import"), ("string", "'./Card.astro'")][..],
            ),
            (
                "html",
                "<script type=\"application/json\">{\"name\": \"世界\", \"count\": 3}</script>",
                &[("string", "\"世界\""), ("constant", "3")][..],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let doc = DocumentSnapshot {
                opening: index as u64,
                revision: 0,
                path: None,
                text: text.into(),
            };
            let run = |syntax: &mut Syntax, query| -> SyntaxQuery {
                let result = syntax.run(language.into(), &doc, Projection::Query(query)).unwrap();
                serde_json::from_value(result).unwrap()
            };
            let plain = run(&mut syntax, plain_query);
            let with_embedded = run(&mut syntax, opt_in_query);
            assert!(plain.complete && with_embedded.complete, "{language}");
            assert!(plain.diagnostics.is_empty() && with_embedded.diagnostics.is_empty());
            assert!(!plain.captures.is_empty(), "{language}");
            assert!(plain.captures.iter().all(|outer| {
                with_embedded.captures.iter().any(|capture| {
                    capture.name == outer.name && capture.range == outer.range
                })
            }));
            for (name, value) in expected {
                let matches = |capture: &Capture| {
                    capture.name == *name && text.get(capture.range.start..capture.range.end) == Some(value)
                };
                assert!(!plain.captures.iter().any(matches), "{language}: {value}");
                assert!(with_embedded.captures.iter().any(matches), "{language}: {value}");
            }
        }
    }
}

fn grammar(name: &str) -> Option<Language> {
    Some(match name {
        "html" | "vue" | "vue-template" => tree_sitter_html::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "javascript" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "astro" => tree_sitter_astro_next::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        "markdown_inline" => tree_sitter_md::INLINE_LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "sql" => tree_sitter_sequel::LANGUAGE.into(),
        "svelte" => tree_sitter_svelte_ng::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "yaml" => tree_sitter_yaml::LANGUAGE.into(),
        _ => return None,
    })
}
