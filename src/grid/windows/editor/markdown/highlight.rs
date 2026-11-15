//! Match Markdown syntax to the source byte ranges used by the editor.

use super::MARKDOWN_OPTIONS;
use nanobug::document::Document;
use nanobug_plugin_sdk::language::{ByteRange, Highlight, SyntaxKind};
use pulldown_cmark::{Event, Parser, Tag};
use regex::Regex;
use std::{
    cell::{Ref, RefCell},
    cmp::Reverse,
    collections::BinaryHeap,
    ops::Range,
    sync::LazyLock,
};

/// Find HTML tag boundaries and comments without coloring the surrounding prose.
static HTML_MARKUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<!--.*?(?:-->|\z)|</?[A-Za-z](?:[^'"<>]|"[^"]*"|'[^']*')*>"#)
        .expect("static HTML markup pattern")
});

/// Retain native Markdown highlights until the document contents change.
#[derive(Default)]
pub(in crate::grid::windows::editor) struct MarkdownHighlights(RefCell<CachedHighlights>);

/// Non-overlapping syntax ranges produced for one revision of an open document.
#[derive(Default)]
struct CachedHighlights {
    /// Opening identity and content revision represented by `spans`.
    version: Option<(u64, u64)>,
    /// Syntax spans sorted by source byte offset.
    spans: Vec<Highlight>,
}

impl MarkdownHighlights {
    /// Return the syntax for `doc`, reusing it across repaints and transient IME composition.
    pub(in crate::grid::windows::editor) fn get(&self, doc: &Document) -> Ref<'_, [Highlight]> {
        let version = Some((doc.opening, doc.revision));
        if self.0.borrow().version != version {
            self.0.replace(CachedHighlights {
                version,
                spans: parse(&doc.text),
            });
        }
        Ref::map(self.0.borrow(), |cached| cached.spans.as_slice())
    }
}

/// Derive syntax highlights from the same Markdown dialect as the rendered preview.
fn parse(source: &str) -> Vec<Highlight> {
    let parser = Parser::new_ext(source, MARKDOWN_OPTIONS);
    let mut regions = Vec::new();
    for (_, definition) in parser.reference_definitions().iter() {
        add(&mut regions, definition.span.clone(), SyntaxKind::Function);
    }
    for (event, range) in parser.into_offset_iter() {
        let kind = match event {
            Event::Start(Tag::Heading { .. }) => SyntaxKind::Keyword,
            Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough) => SyntaxKind::Attribute,
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => SyntaxKind::Function,
            Event::Start(Tag::CodeBlock(_)) | Event::Code(_) => SyntaxKind::String,
            Event::Start(Tag::BlockQuote(_)) => SyntaxKind::Comment,
            Event::FootnoteReference(_) | Event::TaskListMarker(_) | Event::Rule => {
                SyntaxKind::Constant
            }
            Event::Start(Tag::TableHead) => {
                let divider = &source[range.end..];
                let end = divider.find('\n').unwrap_or(divider.len());
                add(
                    &mut regions,
                    range.end..range.end + end,
                    SyntaxKind::Constant,
                );
                SyntaxKind::Type
            }
            Event::Start(Tag::Item) => {
                let item = source[range.clone()].trim_start_matches([' ', '\t']);
                let start = range.end - item.len();
                let marker = item.find(char::is_whitespace).unwrap_or(item.len());
                add(&mut regions, start..start + marker, SyntaxKind::Constant);
                continue;
            }
            Event::Start(Tag::FootnoteDefinition(_)) => {
                if let Some(marker) = source[range.clone()].find("]:") {
                    add(
                        &mut regions,
                        range.start..range.start + marker + 2,
                        SyntaxKind::Constant,
                    );
                }
                continue;
            }
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => {
                for markup in HTML_MARKUP.find_iter(&source[range.clone()]) {
                    let kind = if markup.as_str().starts_with("<!--") {
                        SyntaxKind::Comment
                    } else {
                        SyntaxKind::Type
                    };
                    add(
                        &mut regions,
                        range.start + markup.start()..range.start + markup.end(),
                        kind,
                    );
                }
                continue;
            }
            _ => continue,
        };
        add(&mut regions, range, kind);
    }
    flatten(regions)
}

/// Record a nonempty source `range` and its theme `kind` in `spans`.
fn add(spans: &mut Vec<Highlight>, range: Range<usize>, kind: SyntaxKind) {
    if !range.is_empty() {
        spans.push(Highlight {
            range: ByteRange {
                start: range.start,
                end: range.end,
            },
            kind,
        });
    }
}

/// Flatten potentially nested `regions`, with inner syntax overriding its surrounding span.
fn flatten(mut regions: Vec<Highlight>) -> Vec<Highlight> {
    regions.sort_unstable_by_key(|span| span.range.start);
    // Select the most recently started span, preferring the shorter span at equal starts.
    let mut active: BinaryHeap<(usize, Reverse<usize>, usize)> = BinaryHeap::new();
    let mut result: Vec<Highlight> = Vec::new();
    let mut next = 0;
    let mut offset = 0;
    loop {
        while let Some(span) = regions.get(next).filter(|span| span.range.start <= offset) {
            active.push((span.range.start, Reverse(span.range.end), next));
            next += 1;
        }
        while active.peek().is_some_and(|entry| entry.1.0 <= offset) {
            active.pop();
        }
        let next_start = regions.get(next).map(|span| span.range.start);
        let Some((_, Reverse(end), index)) = active.peek().copied() else {
            let Some(start) = next_start else { break };
            offset = start;
            continue;
        };
        let end = next_start.map_or(end, |start| end.min(start));
        let kind = regions[index].kind;
        if let Some(previous) = result
            .last_mut()
            .filter(|previous| previous.range.end == offset && previous.kind == kind)
        {
            previous.range.end = end;
        } else {
            add(&mut result, offset..end, kind);
        }
        offset = end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mixed Markdown constructs resolve to themed roles and retain their exact UTF-8 bounds.
    #[test]
    fn builtin_markdown_highlights_nested_and_gfm_syntax() {
        let text = concat!(
            "# 标题 **强调**\n\n",
            "[链接](page.md) 和 `代码`\n\n",
            "- [x] 待办\n\n",
            "<p align=\"center\">正文</p>\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "| 标头 |\n| --- |\n| 内容 |\n"
        );
        let spans = parse(text);
        for (token, expected) in [
            ("# 标题", Some(SyntaxKind::Keyword)),
            ("**强调**", Some(SyntaxKind::Attribute)),
            ("[链接]", Some(SyntaxKind::Function)),
            ("`代码`", Some(SyntaxKind::String)),
            ("[x]", Some(SyntaxKind::Constant)),
            ("<p align", Some(SyntaxKind::Type)),
            ("正文", None),
            ("fn main", Some(SyntaxKind::String)),
            ("标头", Some(SyntaxKind::Type)),
            ("| --- |", Some(SyntaxKind::Constant)),
            ("内容", None),
        ] {
            let offset = text.find(token).expect(token);
            let actual = spans
                .iter()
                .find(|span| (span.range.start..span.range.end).contains(&offset))
                .map(|span| span.kind);
            assert_eq!(actual, expected, "{token}");
        }
        assert!(spans.iter().all(|span| {
            text.is_char_boundary(span.range.start) && text.is_char_boundary(span.range.end)
        }));
        assert!(
            spans
                .windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start)
        );
    }

    /// Reparse after an edit rather than leaving a stale span on plain source.
    #[test]
    fn builtin_markdown_highlight_cache_follows_document_revision() {
        let mut doc = Document::new("# Heading");
        let cached = MarkdownHighlights::default();
        assert_eq!(cached.get(&doc)[0].kind, SyntaxKind::Keyword);
        doc.replace(0..2, "", true);
        assert!(cached.get(&doc).is_empty());
    }

    /// Check that built-in syntax reaches the source frame and explicit provider colors win.
    #[test]
    fn builtin_markdown_highlights_reach_the_source_view() {
        use crate::{
            grid::{
                render::Rect,
                windows::{WindowFrame, editor::EditorWindow},
            },
            ui::{
                text::TextSpacing,
                theme::{GOLD, syntax_color},
            },
        };
        use gpui::{FontId, NoopTextSystem};
        use std::sync::Arc;

        let source = "# Heading\n\n[Link](page.md)\n";
        let mut doc = Document::new(source);
        doc.path = Some("README.MD".into());
        let editor = EditorWindow::new(doc);
        let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
        let context = WindowFrame {
            area: Rect {
                w: 88,
                h: 8,
                ..Rect::default()
            },
            selected: false,
            modal_open: false,
            controls_hovered: false,
            spacing: &spacing,
        };
        let (_, actual) = editor.content_frames(context, |_| None);
        let (_, expected) = editor.content_frames(context, |offset| {
            (offset < 9)
                .then(|| syntax_color(SyntaxKind::Keyword))
                .or_else(|| {
                    (11..26)
                        .contains(&offset)
                        .then(|| syntax_color(SyntaxKind::Function))
                })
        });
        assert!(actual == expected);
        let (_, overridden) =
            editor.content_frames(context, |offset| (offset == 0).then_some(GOLD));
        assert!(actual != overridden);
    }
}
