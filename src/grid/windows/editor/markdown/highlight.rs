//! Match Markdown syntax to the source byte ranges used by the editor.

use super::MARKDOWN_OPTIONS;
use nanobug::document::Document;
use nanobug_plugin_sdk::language::{ByteRange, Highlight, SyntaxKind};
use pulldown_cmark::{Event, LinkType, Parser, Tag};
use regex::Regex;
use std::{
    cell::{Ref, RefCell},
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
        let range = definition.span.clone();
        add(&mut regions, range.clone(), SyntaxKind::MarkupLink);
        if let Some(split) = source[range.clone()].find("]:") {
            add_link_url(&mut regions, source, range.start + split + 2..range.end);
        }
    }
    for (event, range) in parser.into_offset_iter() {
        let kind = match event {
            Event::Start(Tag::Heading { .. }) => SyntaxKind::MarkupHeading,
            Event::Start(Tag::Emphasis) => SyntaxKind::MarkupEmphasis,
            Event::Start(Tag::Strong) => SyntaxKind::MarkupStrong,
            Event::Start(Tag::Strikethrough) => SyntaxKind::MarkupStrike,
            Event::Start(Tag::Link { link_type, .. } | Tag::Image { link_type, .. }) => {
                if matches!(link_type, LinkType::Autolink | LinkType::Email) {
                    SyntaxKind::MarkupUrl
                } else {
                    if link_type == LinkType::Inline
                        && let Some(start) = inline_destination(&source[range.clone()])
                    {
                        add_link_url(&mut regions, source, range.start + start..range.end);
                    }
                    SyntaxKind::MarkupLink
                }
            }
            Event::Start(Tag::CodeBlock(_)) => SyntaxKind::MarkupCodeBlock,
            Event::Code(_) => SyntaxKind::MarkupCode,
            Event::Start(Tag::BlockQuote(_)) => SyntaxKind::MarkupQuote,
            Event::TaskListMarker(_) => SyntaxKind::MarkupList,
            Event::FootnoteReference(_) | Event::Rule => SyntaxKind::MarkupMarker,
            Event::Start(Tag::TableHead) => {
                let divider = &source[range.end..];
                let end = divider.find('\n').unwrap_or(divider.len());
                add(
                    &mut regions,
                    range.end..range.end + end,
                    SyntaxKind::MarkupMarker,
                );
                SyntaxKind::MarkupTable
            }
            Event::Start(Tag::Item) => {
                let item = source[range.clone()].trim_start_matches([' ', '\t']);
                let start = range.end - item.len();
                let marker = item.find(char::is_whitespace).unwrap_or(item.len());
                add(&mut regions, start..start + marker, SyntaxKind::MarkupList);
                continue;
            }
            Event::Start(Tag::FootnoteDefinition(_)) => {
                if let Some(marker) = source[range.clone()].find("]:") {
                    add(
                        &mut regions,
                        range.start..range.start + marker + 2,
                        SyntaxKind::MarkupMarker,
                    );
                }
                continue;
            }
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => {
                for markup in HTML_MARKUP.find_iter(&source[range.clone()]) {
                    let kind = if markup.as_str().starts_with("<!--") {
                        SyntaxKind::Comment
                    } else {
                        SyntaxKind::Tag
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
    Highlight::flatten(regions)
}

/// Record a nonempty source `range` and its theme `kind` in `spans`.
fn add(spans: &mut Vec<Highlight>, range: Range<usize>, kind: SyntaxKind) {
    if !range.is_empty() {
        spans.push(Highlight::new(
            ByteRange {
                start: range.start,
                end: range.end,
            },
            kind,
        ));
    }
}

/// Locate the destination after the matching label bracket, including nested images.
fn inline_destination(link: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut chars = link.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && link[i + 1..].starts_with('(') {
                    return Some(i + 2);
                }
            }
            _ => {}
        }
    }
    None
}

/// Highlight the destination only, preserving the link label and optional title.
fn add_link_url(spans: &mut Vec<Highlight>, source: &str, range: Range<usize>) {
    let value = source[range.clone()].trim_start();
    let start = range.end - value.len();
    let angled = value.starts_with('<');
    let mut depth = 0usize;
    let mut chars = value.char_indices().skip(usize::from(angled));
    let mut end = value.len();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '>' if angled => {
                end = i;
                break;
            }
            '(' if !angled => depth += 1,
            ')' if !angled && depth > 0 => depth -= 1,
            c if !angled && (c == ')' || c.is_whitespace()) => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    add(
        spans,
        start + usize::from(angled)..start + end,
        SyntaxKind::MarkupUrl,
    );
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
            ("# 标题", Some(SyntaxKind::MarkupHeading)),
            ("**强调**", Some(SyntaxKind::MarkupStrong)),
            ("[链接]", Some(SyntaxKind::MarkupLink)),
            ("page.md", Some(SyntaxKind::MarkupUrl)),
            ("`代码`", Some(SyntaxKind::MarkupCode)),
            ("[x]", Some(SyntaxKind::MarkupList)),
            ("<p align", Some(SyntaxKind::Tag)),
            ("正文", None),
            ("fn main", Some(SyntaxKind::MarkupCodeBlock)),
            ("标头", Some(SyntaxKind::MarkupTable)),
            ("| --- |", Some(SyntaxKind::MarkupMarker)),
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
        assert_eq!(cached.get(&doc)[0].kind, SyntaxKind::MarkupHeading);
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
                .then(|| syntax_color(SyntaxKind::MarkupHeading))
                .or_else(|| {
                    (18..25)
                        .contains(&offset)
                        .then(|| syntax_color(SyntaxKind::MarkupUrl))
                        .or_else(|| {
                            (11..26)
                                .contains(&offset)
                                .then(|| syntax_color(SyntaxKind::MarkupLink))
                        })
                })
        });
        assert!(actual == expected);
        let (_, overridden) =
            editor.content_frames(context, |offset| (offset == 0).then_some(GOLD));
        assert!(actual != overridden);
    }
}
