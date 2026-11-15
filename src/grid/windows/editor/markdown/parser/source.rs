//! Track which source rows produced the HTML nodes used for Markdown layout.

use super::super::MARKDOWN_OPTIONS;
use ego_tree::{NodeId, Tree};
use html5ever::{QualName, driver, local_name, ns, tendril::TendrilSink};
use pulldown_cmark::{Event, Parser, html};
use scraper::{Html, Node, html::HtmlTreeSink};
use std::{cell::Cell, collections::HashMap, fmt};

/// HTML emitted by the Markdown parser, along with the source row of each created node.
pub(super) struct SourceTree {
    /// Fully parsed fragment, with native HTML recovery and no synthetic markup.
    pub(super) html: Html,
    /// Source rows keyed by the HTML tree's stable node identities.
    pub(super) rows: HashMap<NodeId, usize>,
    /// Number of physical source rows, including an empty final row after a newline.
    pub(super) source_rows: usize,
}

impl SourceTree {
    /// Convert `source` to HTML and attribute each emitted node to the source row that produced it.
    pub(super) fn parse(source: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(offset, _)| offset + 1))
            .collect::<Vec<_>>();
        let row = Cell::new(0);
        let events = Parser::new_ext(source, MARKDOWN_OPTIONS)
            .into_offset_iter()
            .map(|(event, range)| {
                let offset = if matches!(event, Event::End(_)) {
                    range.end.saturating_sub(1)
                } else {
                    range.start
                };
                row.set(starts.partition_point(|&start| start <= offset) - 1);
                event
            });
        let mut markup = Markup {
            row: &row,
            text: String::with_capacity(source.len()),
            chunks: Vec::new(),
        };
        html::write_html_fmt(&mut markup, events).expect("string writes are infallible");

        // Drive the same body-fragment parser as scraper without modifying its HTML input.
        let mut parser = driver::parse_fragment(
            HtmlTreeSink::new(Html::new_fragment()),
            Default::default(),
            QualName::new(None, ns!(html), local_name!("body")),
            Vec::new(),
            false,
        );
        let mut rows = HashMap::new();
        let mut seen = parser.tokenizer.sink.sink.0.borrow().tree.nodes().len();
        let mut last_row = 0;
        for (index, &(start, source_row)) in markup.chunks.iter().enumerate() {
            let end = markup
                .chunks
                .get(index + 1)
                .map_or(markup.text.len(), |&(start, _)| start);
            parser.process(markup.text[start..end].into());
            let html = parser.tokenizer.sink.sink.0.borrow();
            collect_new_nodes(&html.tree, source_row, &mut seen, &mut rows);
            last_row = source_row;
        }
        let html = parser.finish();
        collect_new_nodes(&html.tree, last_row, &mut seen, &mut rows);
        Self {
            html,
            rows,
            source_rows: starts.len(),
        }
    }
}

/// Output writer that remembers where the rendered HTML changes source rows.
struct Markup<'a> {
    /// Row assigned to the current parser event by the iterator.
    row: &'a Cell<usize>,
    /// Exact HTML emitted by pulldown-cmark.
    text: String,
    /// Byte offset and source row at the start of each consecutive chunk.
    chunks: Vec<(usize, usize)>,
}

impl fmt::Write for Markup<'_> {
    /// Record the output location of `text` before appending it unchanged.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let row = self.row.get();
        if !text.is_empty()
            && self
                .chunks
                .last()
                .is_none_or(|&(_, previous)| previous != row)
        {
            self.chunks.push((self.text.len(), row));
        }
        self.text.push_str(text);
        Ok(())
    }
}

/// Record the `row` of any element and text nodes appended after `seen` in the HTML `tree`.
fn collect_new_nodes(
    tree: &Tree<Node>,
    row: usize,
    seen: &mut usize,
    rows: &mut HashMap<NodeId, usize>,
) {
    let count = tree.nodes().len();
    for node in tree.nodes().rev().take(count - *seen) {
        if matches!(node.value(), Node::Element(_) | Node::Text(_)) {
            rows.insert(node.id(), row);
        }
    }
    *seen = count;
}
