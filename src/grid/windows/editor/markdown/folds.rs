//! Derive structural folds from Markdown block boundaries and document heading levels.

use pulldown_cmark::{Event, HeadingLevel, Tag};
use std::ops::Range;

/// Collect nested fold candidates during the source parser's offset event stream.
pub(super) struct SourceFolds<'a> {
    /// The source that bounds all parser offsets.
    source: &'a str,
    /// Number of unclosed parser tags, used to identify document-level headings.
    depth: usize,
    /// Open sections, tracked by heading level and opening byte.
    headings: Vec<(HeadingLevel, usize)>,
    /// Completed section and block ranges in source bytes, including the headers.
    ranges: Vec<Range<usize>>,
}

impl<'a> SourceFolds<'a> {
    /// Begin collecting folds from `source`.
    pub(super) fn new(source: &'a str) -> Self {
        Self {
            source,
            depth: 0,
            headings: Vec::new(),
            ranges: Vec::new(),
        }
    }

    /// Record the structure of a Markdown parser `event` covering `range` in the source.
    pub(super) fn observe(&mut self, event: &Event<'_>, range: &Range<usize>) {
        match event {
            Event::Start(tag) => {
                match tag {
                    Tag::Heading { level, .. } if self.depth == 0 => {
                        // Headings inside list items or quotes belong to their enclosing block.
                        while self
                            .headings
                            .last()
                            .is_some_and(|&(open, _)| open >= *level)
                        {
                            let (_, start) = self.headings.pop().unwrap();
                            self.add(start..range.start);
                        }
                        self.headings.push((*level, range.start));
                    }
                    Tag::BlockQuote(_)
                    | Tag::CodeBlock(_)
                    | Tag::HtmlBlock
                    | Tag::List(_)
                    | Tag::Item
                    | Tag::FootnoteDefinition(_)
                    | Tag::Table(_) => self.add(range.clone()),
                    _ => {}
                }
                self.depth += 1;
            }
            Event::End(_) => self.depth -= 1,
            _ => {}
        }
    }

    /// Finish sections reaching the end of the source and return the collected fold ranges.
    pub(super) fn finish(mut self) -> Vec<Range<usize>> {
        while let Some((_, start)) = self.headings.pop() {
            self.add(start..self.source.len());
        }
        self.ranges
    }

    /// Add a source `range`, leaving separator blank lines outside the fold.
    fn add(&mut self, range: Range<usize>) {
        let text = self.source[range.clone()].trim_end_matches([' ', '\t', '\r', '\n']);
        self.ranges.push(range.start..range.start + text.len());
    }
}
