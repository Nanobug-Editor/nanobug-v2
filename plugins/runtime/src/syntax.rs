//! Shared, bounded Tree-sitter parsing in its own worker role.

use crate::Output;
use nanobug_plugin_sdk::{
    Diagnostic, DocumentSnapshot, ErrorKind, Reply, Request,
    language::{ByteRange, Capture, SyntaxQuery},
    receive,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::BufRead,
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{
    Language, Node, Parser, Query, QueryCursor, QueryCursorOptions, StreamingIterator, Tree,
};

const MAX_NODES: usize = 2048;
/// Cap both capture traffic and syntax error rows for a single snapshot.
const MAX_CAPTURES: usize = 6144;
const MAX_DIAGNOSTICS: usize = 80;
const MAX_EMBEDDED_RANGES: usize = 32;
const MAX_CACHED: usize = 12;
const PARSE_BUDGET: Duration = Duration::from_millis(500);
type Key = (String, u64, u64);

/// Reuse parse results for the same document version and grammar.
pub(super) fn serve(mut input: impl BufRead, output: Output) -> anyhow::Result<()> {
    let mut syntax = Syntax::default();
    while let Some(request) = receive(&mut input)? {
        let (id, language, document, query) = match request {
            Request::Parse {
                id,
                language,
                document,
            } => (id, language, document, None),
            Request::QuerySyntax {
                id,
                language,
                query,
                document,
            } => (id, language, document, Some(query)),
            _ => anyhow::bail!("unexpected syntax request"),
        };
        match syntax.run(language, &document, query.as_deref()) {
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
}

impl Syntax {
    /// Parse or reuse the tree for `doc` and project it as a node list or a bounded `query`.
    fn run(
        &mut self,
        language: String,
        doc: &DocumentSnapshot,
        query: Option<&str>,
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
        let result = if let Some(source) = query {
            self.query(
                &entry.0.0,
                &base,
                source,
                entry.1.root_node(),
                doc,
                deadline,
            )
        } else if let Some(value) = &entry.2 {
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
        let result = capture(&entry.2, root, doc, deadline);
        self.queries.push_back(entry);
        result
    }
}

/// Return query captures and parser recovery errors in UTF-8 byte coordinates.
fn capture(
    query: &Query,
    root: Node<'_>,
    doc: &DocumentSnapshot,
    deadline: Instant,
) -> anyhow::Result<Value> {
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
    let mut matches = cursor.captures_with_options(query, root, doc.text.as_bytes(), options);
    let mut captures = Vec::new();
    while captures.len() < MAX_CAPTURES {
        let Some((matched, index)) = matches.next() else {
            break;
        };
        let item = matched.captures[*index];
        captures.push(Capture {
            name: query.capture_names()[item.index as usize].into(),
            range: ByteRange {
                start: item.node.start_byte(),
                end: item.node.end_byte(),
            },
        });
    }
    let complete = captures.len() < MAX_CAPTURES;
    drop(matches);
    anyhow::ensure!(Instant::now() < deadline, "Syntax query timed out");
    let diagnostics = descendants(root)
        .take_while(|_| Instant::now() < deadline)
        .filter(|node| node.is_error() || node.is_missing())
        .take(MAX_DIAGNOSTICS)
        .map(|node| Diagnostic {
            start: node.start_byte(),
            end: node.end_byte(),
            message: if node.is_missing() {
                format!("Expected `{}`", node.kind())
            } else {
                "Unexpected syntax".into()
            },
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(Instant::now() < deadline, "Syntax diagnostics timed out");
    Ok(serde_json::to_value(SyntaxQuery {
        opening: doc.opening,
        revision: doc.revision,
        complete: complete
            && diagnostics.len() < MAX_DIAGNOSTICS
            && !cursor.did_exceed_match_limit(),
        captures,
        diagnostics,
    })?)
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

fn grammar(name: &str) -> Option<Language> {
    Some(match name {
        "html" | "vue" | "vue-template" => tree_sitter_html::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => return None,
    })
}
