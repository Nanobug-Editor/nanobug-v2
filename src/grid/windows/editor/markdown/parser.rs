//! Converts Markdown and embedded HTML into inert content for the native preview.

mod images;
mod layout;
mod source;

use ego_tree::{NodeId, NodeRef};
pub(super) use images::{ImageSize, ImageWidth};
use scraper::{ElementRef, Node};
use source::SourceTree;
use std::collections::HashMap;

pub(super) use layout::Alignment;
use layout::{block_alignment, image_alignment, style_value, text_alignment};

/// Inline content laid out together in a paragraph or table cell.
#[derive(Debug, PartialEq)]
pub(super) struct AlignedText {
    /// Styled runs and objects in document order.
    pub inlines: Vec<Inline>,
    /// Effective horizontal alignment after inheritance.
    pub alignment: Alignment,
}

/// Parsed blocks and the source extent needed to map preview offsets to the editor.
pub(super) struct Markdown {
    /// Visible blocks in preview order.
    pub(super) blocks: Vec<LocatedBlock>,
    /// Physical document rows, including an empty row after a terminal newline.
    pub(super) source_rows: usize,
}

/// A block and its first physical source row when one can be identified.
#[derive(Debug, PartialEq)]
pub(super) struct LocatedBlock {
    /// Renderable block content.
    pub(super) block: Block,
    /// Zero-based source row of the block or explicit HTML container that produced it.
    pub(super) source_row: Option<usize>,
}

/// The layout units produced by the Markdown preview parser.
#[derive(Debug, PartialEq)]
pub(super) enum Block {
    /// A level 1–6 heading with styled text and an optional explicit HTML ID.
    Heading {
        /// Heading depth, from 1 through 6.
        level: u8,
        /// Visible title content used to derive an automatic anchor.
        inlines: Vec<Inline>,
        /// Explicit destination supplied by an HTML heading.
        id: Option<String>,
        /// Effective horizontal alignment of the heading text.
        alignment: Alignment,
    },
    /// A paragraph of styled text.
    Paragraph(AlignedText),
    /// A preformatted block with its source whitespace preserved.
    Code {
        /// Source text before tab expansion and line wrapping.
        content: String,
        /// Effective horizontal alignment of its lines.
        alignment: Alignment,
        /// Source row at each line start before wrapping or tab expansion.
        line_sources: Vec<Option<usize>>,
    },
    /// A quoted sequence of blocks.
    Quote(Vec<LocatedBlock>),
    /// An ordered list with a starting number, or an unordered list.
    List {
        start: Option<u64>,
        items: Vec<Vec<LocatedBlock>>,
    },
    /// A thematic break.
    Rule,
    /// A table with optional header cells and individually aligned cells.
    Table {
        /// Styled cells in the header row, when present.
        headings: Vec<AlignedText>,
        /// Styled cells grouped by their body row.
        rows: Vec<Vec<AlignedText>>,
        /// Source row of each table row, including the header when present.
        row_sources: Vec<Option<usize>>,
    },
}

/// Styles inherited by text inside nested inline markup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct InlineStyle {
    /// Strong emphasis.
    pub bold: bool,
    /// Regular emphasis.
    pub italic: bool,
    /// Deleted or struck text.
    pub strike: bool,
    /// Inline preformatted code.
    pub code: bool,
    /// The validated link destination.
    pub link: Option<String>,
}

/// A text run or inline object in a paragraph, heading, or table cell.
#[derive(Debug, PartialEq)]
pub(super) enum Inline {
    /// Contiguous text with a shared style.
    Text { content: String, style: InlineStyle },
    /// A forced line break.
    Break,
    /// An image, optionally wrapped in a link.
    Image {
        /// Fallback text displayed if the image cannot load.
        alt: String,
        /// Validated image source.
        url: Option<String>,
        /// Validated destination inherited from a surrounding link.
        link: Option<String>,
        /// Explicit HTML dimensions, if present.
        size: ImageSize,
        /// Position override for an HTML image, or inherited paragraph alignment.
        alignment: Option<Alignment>,
    },
    /// Zero-width destination defined by a named HTML anchor.
    Anchor(String),
    /// A read-only task marker, true for a completed task.
    Checkbox(bool),
}

/// Bounds recursion while translating deeply nested HTML into preview blocks.
const MAX_NESTING: usize = 128;

/// Parses the unsaved Markdown `source` and its HTML into native preview blocks.
pub(super) fn parse(source: &str) -> Markdown {
    let source = SourceTree::parse(source);
    let blocks = blocks(
        source.html.root_element(),
        &InlineStyle::default(),
        Alignment::Left,
        0,
        &source.rows,
    );
    Markdown {
        blocks,
        source_rows: source.source_rows,
    }
}

/// A node borrowed from the parsed fragment.
type HtmlNode<'a> = NodeRef<'a, Node>;

/// Collects blocks and groups adjacent inline content into implicit paragraphs.
struct Blocks<'a> {
    /// Complete blocks emitted in document order.
    ready: Vec<LocatedBlock>,
    /// Inline content awaiting a paragraph boundary.
    paragraph: Inlines,
    /// Source row of the material currently being collected into a paragraph.
    paragraph_row: Option<usize>,
    /// Effective paragraph alignment for the current HTML container.
    alignment: Alignment,
    /// Rows at which the HTML parser created source nodes.
    rows: &'a HashMap<NodeId, usize>,
}

/// Build blocks from `parent` with inherited `style`, `alignment` and nesting `depth`.
fn blocks(
    parent: ElementRef<'_>,
    style: &InlineStyle,
    alignment: Alignment,
    depth: usize,
    rows: &HashMap<NodeId, usize>,
) -> Vec<LocatedBlock> {
    let mut output = Blocks {
        ready: Vec::new(),
        paragraph: Inlines::default(),
        paragraph_row: rows.get(&parent.id()).copied(),
        alignment: block_alignment(parent, alignment),
        rows,
    };
    output.paragraph.anchor(parent.attr("id"));
    for child in parent.children() {
        output.visit(child, style, depth + 1);
    }
    output.finish_paragraph();
    output.ready
}

impl Blocks<'_> {
    /// Ends the current implicit paragraph if it contains visible content.
    fn finish_paragraph(&mut self) {
        let inlines = std::mem::take(&mut self.paragraph).ready;
        let source_row = self.paragraph_row.take();
        if !inlines.is_empty() {
            self.ready.push(LocatedBlock {
                block: Block::Paragraph(AlignedText {
                    inlines,
                    alignment: self.alignment,
                }),
                source_row,
            });
        }
    }

    /// Visits a `node` with inherited `style` at the given nesting `depth`.
    fn visit(&mut self, node: HtmlNode<'_>, style: &InlineStyle, depth: usize) {
        if depth > MAX_NESTING {
            return;
        }
        let source_row = self.rows.get(&node.id()).copied();
        let Some(element) = ElementRef::wrap(node) else {
            if let Node::Text(text) = node.value() {
                self.paragraph.text(text, style);
                if !self.paragraph.ready.is_empty() {
                    self.paragraph_row = self.paragraph_row.or(source_row);
                }
            }
            return;
        };

        if omitted(element) {
            return;
        }
        let name = element.value().name();
        let block = match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Block::Heading {
                level: name.as_bytes()[1] - b'0',
                inlines: inlines(element, style, depth),
                id: element
                    .attr("id")
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned),
                alignment: block_alignment(element, self.alignment),
            },
            "pre" => {
                let mut code = Preformatted::default();
                preformatted(element, &mut code, depth, self.rows);
                Block::Code {
                    content: code.content,
                    alignment: block_alignment(element, self.alignment),
                    line_sources: code.line_sources,
                }
            }
            "blockquote" => quote(element, style, self.alignment, depth, self.rows),
            "ol" | "ul" | "menu" => list(element, style, self.alignment, depth, self.rows),
            "hr" => Block::Rule,
            "table" => table(element, style, self.alignment, depth, self.rows),
            "address" | "article" | "aside" | "center" | "dd" | "details" | "div" | "dl" | "dt"
            | "figcaption" | "figure" | "footer" | "header" | "main" | "nav" | "p" | "section"
            | "summary" => {
                self.finish_paragraph();
                self.ready
                    .extend(blocks(element, style, self.alignment, depth, self.rows));
                return;
            }
            _ => {
                self.paragraph_row = self.paragraph_row.or(source_row);
                if let Some(style) = inline_children(element, style, &mut self.paragraph) {
                    for child in element.children() {
                        self.visit(child, &style, depth + 1);
                    }
                }
                return;
            }
        };
        self.finish_paragraph();
        self.ready.push(LocatedBlock { block, source_row });
    }
}

/// Collected preformatted text and the source row supplying each rendered line.
#[derive(Default)]
struct Preformatted {
    /// Renderable text with original spaces and tabs.
    content: String,
    /// Origin of each line before layout wraps it.
    line_sources: Vec<Option<usize>>,
}

impl Preformatted {
    /// Append `text` from `row`, preserving the first contributing source row of every line.
    fn append(&mut self, text: &str, row: Option<usize>) {
        for (index, line) in text.split_inclusive('\n').enumerate() {
            if self.content.is_empty() || self.content.ends_with('\n') {
                self.line_sources.push(row.map(|row| row + index));
            }
            self.content.push_str(line);
        }
    }
}

/// Append `element`'s code text and line breaks, using `rows` to locate them and bounding `depth`.
fn preformatted(
    element: ElementRef<'_>,
    code: &mut Preformatted,
    depth: usize,
    rows: &HashMap<NodeId, usize>,
) {
    if depth == MAX_NESTING {
        return;
    }
    for child in element.children() {
        let source_row = rows.get(&child.id()).copied();
        if let Node::Text(text) = child.value() {
            code.append(text, source_row);
        } else if let Some(element) = ElementRef::wrap(child)
            && !omitted(element)
        {
            if element.value().name() == "br" {
                code.append("\n", source_row);
            } else {
                preformatted(element, code, depth + 1, rows);
            }
        }
    }
}

/// Builds a quote from its `element`, retaining a GitHub alert's type label.
/// The surrounding `style`, `alignment`, `depth` and source `rows` carry into the quote.
fn quote(
    element: ElementRef<'_>,
    style: &InlineStyle,
    alignment: Alignment,
    depth: usize,
    rows: &HashMap<NodeId, usize>,
) -> Block {
    let mut contents = blocks(element, style, alignment, depth, rows);
    let label = element.value().classes().find_map(|class| match class {
        "markdown-alert-note" => Some("Note"),
        "markdown-alert-tip" => Some("Tip"),
        "markdown-alert-important" => Some("Important"),
        "markdown-alert-warning" => Some("Warning"),
        "markdown-alert-caution" => Some("Caution"),
        _ => None,
    });
    if let Some(label) = label {
        contents.insert(
            0,
            LocatedBlock {
                block: Block::Paragraph(AlignedText {
                    inlines: vec![Inline::Text {
                        content: label.into(),
                        style: InlineStyle {
                            bold: true,
                            ..style.clone()
                        },
                    }],
                    alignment: block_alignment(element, alignment),
                }),
                source_row: None,
            },
        );
    }
    Block::Quote(contents)
}

/// Build items from `element` with inherited `style`, `alignment`, `depth` and source `rows`.
fn list(
    element: ElementRef<'_>,
    style: &InlineStyle,
    alignment: Alignment,
    depth: usize,
    rows: &HashMap<NodeId, usize>,
) -> Block {
    let start = (element.value().name() == "ol").then(|| {
        element
            .attr("start")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1)
    });
    let alignment = block_alignment(element, alignment);
    let items = element
        .child_elements()
        .filter(|item| item.value().name() == "li" && !omitted(*item))
        .map(|item| blocks(item, style, alignment, depth + 1, rows))
        .collect();
    Block::List { start, items }
}

/// Convert `element` with `style`, `alignment` and `depth`, recording displayed table row sources.
fn table(
    element: ElementRef<'_>,
    style: &InlineStyle,
    alignment: Alignment,
    depth: usize,
    source: &HashMap<NodeId, usize>,
) -> Block {
    let mut headings = Vec::new();
    let mut header_source = None;
    let mut rows = Vec::new();
    let mut row_sources = Vec::new();
    let alignment = text_alignment(element).unwrap_or(alignment);
    for section in element.child_elements().filter(|section| {
        matches!(section.value().name(), "thead" | "tbody" | "tfoot") && !omitted(*section)
    }) {
        let alignment = block_alignment(section, alignment);
        for row in section
            .child_elements()
            .filter(|row| row.value().name() == "tr" && !omitted(*row))
        {
            let alignment = block_alignment(row, alignment);
            let cells: Vec<_> = row
                .child_elements()
                .filter(|cell| matches!(cell.value().name(), "th" | "td") && !omitted(*cell))
                .collect();
            let is_first = headings.is_empty() && rows.is_empty();
            let is_header = is_first
                && (section.value().name() == "thead"
                    || cells.iter().all(|cell| cell.value().name() == "th"));
            let contents = cells
                .into_iter()
                .map(|cell| AlignedText {
                    inlines: inlines(cell, style, depth + 3),
                    alignment: block_alignment(cell, alignment),
                })
                .collect();
            if is_header {
                headings = contents;
                header_source = source.get(&row.id()).copied();
            } else {
                rows.push(contents);
                row_sources.push(source.get(&row.id()).copied());
            }
        }
    }
    if !headings.is_empty() {
        row_sources.insert(0, header_source);
    }
    Block::Table {
        headings,
        rows,
        row_sources,
    }
}

/// Groups styled inline content and collapses HTML whitespace across adjacent tags.
#[derive(Default)]
struct Inlines {
    /// Runs and objects emitted in document order.
    ready: Vec<Inline>,
    /// Style of collapsed whitespace to emit only if another visible token occurs.
    pending_space: Option<InlineStyle>,
}

impl Inlines {
    /// Appends visible `content` with the given `style`, merging adjacent text runs.
    fn run(&mut self, content: String, style: &InlineStyle) {
        if content.is_empty() {
            return;
        }
        if let Some(Inline::Text {
            content: previous,
            style: previous_style,
        }) = self.ready.last_mut()
            && previous_style == style
        {
            previous.push_str(&content);
        } else {
            self.ready.push(Inline::Text {
                content,
                style: style.clone(),
            });
        }
    }

    /// Emits a pending space before the next run or inline object.
    fn flush_space(&mut self) {
        if let Some(style) = self.pending_space.take() {
            self.run(" ".into(), &style);
        }
    }

    /// Appends raw `text`, preserving code spacing and collapsing HTML whitespace elsewhere.
    fn text(&mut self, text: &str, style: &InlineStyle) {
        if style.code {
            self.flush_space();
            self.run(text.replace(['\t', '\n', '\x0C', '\r'], " "), style);
            return;
        }
        let mut normalized = String::with_capacity(text.len());
        let last_visible = self
            .ready
            .iter()
            .rev()
            .find(|inline| !matches!(inline, Inline::Anchor(_)));
        let mut after_content = !matches!(last_visible, None | Some(Inline::Break));
        for ch in text.chars() {
            if matches!(ch, '\t' | '\n' | '\x0C' | '\r' | ' ') {
                if after_content {
                    self.pending_space.get_or_insert_with(|| style.clone());
                }
                continue;
            }
            if normalized.is_empty() {
                self.flush_space();
            } else if self.pending_space.take().is_some() {
                normalized.push(' ');
            }
            normalized.push(ch);
            after_content = true;
        }
        self.run(normalized, style);
    }

    /// Ends the line without retaining pending whitespace.
    fn line_break(&mut self) {
        self.pending_space = None;
        self.ready.push(Inline::Break);
    }

    /// Emits an inline `object` after a pending word separator.
    fn object(&mut self, object: Inline) {
        self.flush_space();
        self.ready.push(object);
    }

    /// Record a named destination at the current text position, without emitting whitespace.
    fn anchor(&mut self, id: Option<&str>) {
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            self.ready.push(Inline::Anchor(id.into()));
        }
    }

    /// Visits an inline `node` using inherited `style` and nesting `depth`.
    fn visit(&mut self, node: HtmlNode<'_>, style: &InlineStyle, depth: usize) {
        if depth > MAX_NESTING {
            return;
        }
        if let Node::Text(text) = node.value() {
            self.text(text, style);
        } else if let Some(element) = ElementRef::wrap(node)
            && !omitted(element)
            && let Some(style) = inline_children(element, style, self)
        {
            for child in element.children() {
                self.visit(child, &style, depth + 1);
            }
        }
    }
}

/// Identifies hidden content and elements outside the native preview's static HTML subset.
fn omitted(element: ElementRef<'_>) -> bool {
    element.attr("hidden").is_some()
        || style_value(element, "display", Some)
            .is_some_and(|value| value.eq_ignore_ascii_case("none"))
        || matches!(
            element.value().name(),
            "head"
                | "title"
                | "script"
                | "style"
                | "template"
                | "iframe"
                | "object"
                | "embed"
                | "svg"
                | "math"
                | "textarea"
                | "select"
                | "button"
                | "noscript"
                | "audio"
                | "video"
        )
}

/// Collects styled inline content inside `parent` with its inherited `style` and `depth`.
fn inlines(parent: ElementRef<'_>, style: &InlineStyle, depth: usize) -> Vec<Inline> {
    let mut output = Inlines::default();
    for child in parent.children() {
        output.visit(child, style, depth + 1);
    }
    output.ready
}

/// Emits an inline leaf into `output` or returns the style its `element` children inherit.
/// The parent `style` supplies the styles of surrounding markup.
fn inline_children(
    element: ElementRef<'_>,
    style: &InlineStyle,
    output: &mut Inlines,
) -> Option<InlineStyle> {
    let mut inherited = style.clone();
    output.anchor(element.attr("id"));
    match element.value().name() {
        "strong" | "b" => inherited.bold = true,
        "em" | "i" => inherited.italic = true,
        "del" | "s" | "strike" => inherited.strike = true,
        "code" | "kbd" | "samp" | "tt" => inherited.code = true,
        "a" => {
            inherited.link = safe_url(element.attr("href"), true);
            output.anchor(element.attr("name"));
        }
        "br" => {
            output.line_break();
            return None;
        }
        "img" => {
            output.object(Inline::Image {
                alt: element.attr("alt").unwrap_or_default().into(),
                url: safe_url(element.attr("src"), false),
                link: inherited.link,
                size: ImageSize::parse(element),
                alignment: image_alignment(element),
            });
            return None;
        }
        "input" => {
            if element
                .attr("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("checkbox"))
            {
                output.object(Inline::Checkbox(element.attr("checked").is_some()));
            }
            return None;
        }
        _ => {}
    }
    Some(inherited)
}

/// Accepts a `value` only for relative destinations or explicit web URLs.
/// Set `allow_email` for links that may use the mailto scheme.
fn safe_url(value: Option<&str>, allow_email: bool) -> Option<String> {
    let value = value?.trim();
    if value.is_empty()
        || value.starts_with("//")
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return None;
    }
    if value
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|part| part.contains(':'))
    {
        let parsed = url::Url::parse(value).ok()?;
        match parsed.scheme() {
            "http" | "https" if parsed.has_host() => {}
            "mailto" if allow_email => {}
            _ => return None,
        }
    }
    Some(value.into())
}

#[cfg(test)]
mod tests;
