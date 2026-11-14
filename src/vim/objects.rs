//! Text objects and structural motion targets.

use super::{
    Vim,
    motions::{Kind, Motion},
};
use regex::Regex;
use std::{ops::Range, sync::LazyLock};

/// Markup tokens needed to pair explicit HTML/XML tags, skipping the contents of attributes.
static TAGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<!--.*?-->|<!\[CDATA\[.*?\]\]>|<\?.*?\?>|</?([A-Za-z_][\w:.-]*)(?:[^<>'"]|'[^']*'|"[^"]*")*>"#)
        .expect("valid tag pattern")
});

impl Vim {
    /// Resolve a text object and its `around` variant, with the requested `count`.
    /// An inner-word count past the end leaves the cursor at the file end.
    pub(super) fn object(
        &mut self,
        key: &str,
        around: bool,
        count: usize,
    ) -> Option<(Range<usize>, Kind)> {
        let cursor = self.doc.cursor;
        let row = self.doc.location(cursor).0;
        match key {
            "s" => self
                .sentence_object(around, count)
                .map(|range| (range, Kind::Exclusive)),
            "t" => self
                .tag_object(around, count)
                .map(|range| (range, Kind::Exclusive)),
            "w" | "W" => {
                let big = key == "W";
                let mut start = cursor;
                let class = self.class(start, big);
                while start > 0
                    && self.doc.text.as_bytes()[start - 1] != b'\n'
                    && self.class(self.doc.previous(start), big) == class
                {
                    start = self.doc.previous(start);
                }
                let mut end = cursor;
                while end < self.doc.text.len() && self.class(end, big) == class {
                    if self.doc.text.as_bytes()[end] == b'\n' {
                        break;
                    }
                    end = self.doc.next(end);
                }
                if !around {
                    return self.extend_inner_word(start..end, big, count);
                }
                for _ in 1..count {
                    let next = if self.class(end, big) == 0 {
                        self.word_forward(end, big)
                    } else {
                        end
                    };
                    if next == self.doc.text.len() {
                        break;
                    }
                    let class = self.class(next, big);
                    end = next;
                    while end < self.doc.text.len() && self.class(end, big) == class {
                        end = self.doc.next(end);
                    }
                }
                if class != 0 {
                    let line = self.doc.line_range(self.doc.location(end).0);
                    let old_end = end;
                    while end < line.end && self.class(end, big) == 0 {
                        end = self.doc.next(end);
                    }
                    if end == old_end {
                        while start > self.doc.starts[row]
                            && self.class(self.doc.previous(start), big) == 0
                        {
                            start = self.doc.previous(start);
                        }
                    }
                }
                Some((start..end, Kind::Exclusive))
            }
            "p" => {
                let blank = self.doc.line(row).is_empty();
                let mut first = row;
                while first > 0 && self.doc.line(first - 1).is_empty() == blank {
                    first -= 1;
                }
                let mut last = row;
                let max = self.line_count() - 1;
                for i in 0..count {
                    if i > 0 {
                        while last < max && self.doc.line(last + 1).is_empty() {
                            last += 1;
                        }
                        if last < max {
                            last += 1;
                        }
                    }
                    while last < max && self.doc.line(last + 1).is_empty() == (blank && i == 0) {
                        last += 1;
                    }
                }
                if around {
                    while last < max && self.doc.line(last + 1).is_empty() {
                        last += 1;
                    }
                }
                Some((self.line_span(first, last), Kind::Line))
            }
            "\"" | "'" | "`" => {
                let quote = key.chars().next()?;
                let range = self.doc.line_range(row);
                let mut opening = None;
                let mut escaped = false;
                for (i, ch) in self.doc.text[range.clone()].char_indices() {
                    if ch == quote && !escaped {
                        let at = range.start + i;
                        if let Some(start) = opening.take() {
                            if at >= cursor {
                                return Some((
                                    if around { start..at + 1 } else { start + 1..at },
                                    Kind::Exclusive,
                                ));
                            }
                        } else {
                            opening = Some(at);
                        }
                    }
                    escaped = ch == '\\' && !escaped;
                }
                None
            }
            "(" | ")" | "b" | "{" | "}" | "B" | "[" | "]" | "<" | ">" => {
                let (open, close) = match key {
                    "(" | ")" | "b" => ('(', ')'),
                    "{" | "}" | "B" => ('{', '}'),
                    "[" | "]" => ('[', ']'),
                    _ => ('<', '>'),
                };
                let mut skip = count - 1;
                for (start, _) in self.doc.text[..self.doc.next(cursor)]
                    .match_indices(open)
                    .rev()
                {
                    if let Some(end) = self.match_bracket(start, open, close, false)
                        && end >= cursor
                    {
                        if skip == 0 {
                            return Some((
                                if around {
                                    start..end + 1
                                } else {
                                    start + 1..end
                                },
                                Kind::Exclusive,
                            ));
                        }
                        skip -= 1;
                    }
                }
                let line = self.doc.line_range(row);
                let start = cursor + self.doc.text[cursor..line.end].find(open)?;
                let end = self.match_bracket(start, open, close, false)?;
                (count == 1).then_some((
                    if around {
                        start..end + 1
                    } else {
                        start + 1..end
                    },
                    Kind::Exclusive,
                ))
            }
            _ => None,
        }
    }

    /// Select the sentence at the caret and up to `count` sentences including it.
    /// `around` includes the following whitespace, or the preceding gap at paragraph end.
    fn sentence_object(&self, around: bool, count: usize) -> Option<Range<usize>> {
        let text = &self.doc.text;
        let mut starts = self.sentence_starts();
        starts.push(text.len());
        starts.dedup();
        let cursor = self.doc.cursor;
        let index = starts
            .partition_point(|&start| start <= cursor)
            .checked_sub(1)?;
        let next = *starts.get(index + 1)?;
        let start = starts[index];
        let content_end = start
            + text[start..next]
                .trim_end_matches(char::is_whitespace)
                .len();
        if !around && cursor >= content_end && cursor < next && count == 1 {
            return Some(content_end..next);
        }
        let limit = index.saturating_add(count).min(starts.len() - 1);
        let end = starts[limit];
        let trimmed = start + text[start..end].trim_end_matches(char::is_whitespace).len();
        let leading = starts
            .get(index.wrapping_sub(1))
            .map_or(start, |&previous| {
                previous
                    + text[previous..start]
                        .trim_end_matches(char::is_whitespace)
                        .len()
            });
        Some(if !around {
            start..trimmed
        } else if trimmed < end {
            start..end
        } else {
            leading..end
        })
    }

    /// Select the `count`th enclosing paired tag, including markup when `around` is set.
    fn tag_object(&self, around: bool, count: usize) -> Option<Range<usize>> {
        let mut open = Vec::<(&str, Range<usize>)>::new();
        let mut enclosing = Vec::new();
        for token in TAGS.captures_iter(&self.doc.text) {
            let Some(name) = token.get(1) else { continue };
            let tag = token.get(0)?;
            if tag.as_str().starts_with("</") {
                let Some(index) = open.iter().rposition(|(name_at_open, _)| {
                    name_at_open.eq_ignore_ascii_case(name.as_str())
                }) else {
                    continue;
                };
                let (_, start) = open.remove(index);
                open.truncate(index);
                if (start.start..tag.end()).contains(&self.doc.cursor) {
                    let range = if around {
                        start.start..tag.end()
                    } else {
                        start.end..tag.start()
                    };
                    enclosing.push(range);
                }
            } else if !tag.as_str().ends_with("/>") {
                open.push((name.as_str(), tag.range()));
            }
        }
        enclosing.into_iter().nth(count.saturating_sub(1))
    }

    /// Extend the initial word `range`, counting inline whitespace as its own segment.
    /// `big` treats punctuation as part of a WORD. Empty lines also form a segment.
    fn extend_inner_word(
        &mut self,
        mut range: Range<usize>,
        big: bool,
        count: usize,
    ) -> Option<(Range<usize>, Kind)> {
        for _ in 1..count {
            let row = self.doc.location(range.end).0;
            let mut limit = self.doc.line_range(row).end;
            if range.end == limit {
                if row + 1 >= self.line_count() {
                    let last = self.doc.line_range(self.line_count() - 1);
                    self.set_cursor(self.doc.previous(last.end).max(last.start));
                    return None;
                }
                range.end = self.doc.starts[row + 1];
                limit = self.doc.line_range(row + 1).end;
                if range.end == limit {
                    range.end = self.doc.next(range.end);
                    continue;
                }
            }
            let class = self.class(range.end, big);
            while range.end < limit && self.class(range.end, big) == class {
                range.end = self.doc.next(range.end);
            }
        }
        let first = self.doc.location(range.start).0;
        let last = self.doc.location(range.end).0;
        if !self.mode.is_visual() && first < last && range.end == self.doc.starts[last] {
            // An exclusive end at column one can promote the operation to whole lines.
            return if range.start <= self.first_nonblank(first) {
                Some((self.line_span(first, last - 1), Kind::Line))
            } else {
                Some((
                    range.start..self.doc.line_range(last - 1).end,
                    Kind::Exclusive,
                ))
            };
        }
        Some((range, Kind::Exclusive))
    }

    /// Match the default bracket pair starting at byte `at`.
    pub(super) fn paired(&self, at: usize) -> Option<usize> {
        let ch = self.doc.text[at..].chars().next()?;
        let (open, close) = match ch {
            '(' | ')' => ('(', ')'),
            '[' | ']' => ('[', ']'),
            '{' | '}' => ('{', '}'),
            _ => return None,
        };
        self.match_bracket(at, ch, if ch == open { close } else { open }, ch == close)
    }

    /// Scan from `at` for `close` at nesting depth zero.
    fn match_bracket(&self, at: usize, open: char, close: char, backward: bool) -> Option<usize> {
        let mut depth = 0;
        let mut step = |ch| {
            if ch == open {
                depth += 1;
            }
            if ch == close {
                depth -= 1;
            }
            depth == 0
        };
        if backward {
            self.doc.text[..at + 1]
                .char_indices()
                .rev()
                .find(|&(_, ch)| step(ch))
                .map(|(i, _)| i)
        } else {
            self.doc.text[at..]
                .char_indices()
                .find(|&(_, ch)| step(ch))
                .map(|(i, _)| at + i)
        }
    }

    /// Move to the next or previous empty line delimiting a paragraph.
    pub(super) fn paragraph_motion(&self, backward: bool, count: usize) -> usize {
        let mut row = self.doc.location(self.doc.cursor).0;
        let last = self.line_count() - 1;
        for _ in 0..count {
            if backward {
                while row > 0 && self.doc.line(row).is_empty() {
                    row -= 1;
                }
                row = row.saturating_sub(1);
                while row > 0 && !self.doc.line(row).is_empty() {
                    row -= 1;
                }
            } else {
                while row < last && self.doc.line(row).is_empty() {
                    row += 1;
                }
                if row < last {
                    row += 1;
                }
                while row < last && !self.doc.line(row).is_empty() {
                    row += 1;
                }
                if row == last && !self.doc.line(row).is_empty() {
                    return self.doc.line_range(row).end;
                }
            }
        }
        self.doc.starts[row]
    }

    /// Move among sentence beginnings, recognizing punctuation and paragraph breaks.
    pub(super) fn sentence_motion(&self, backward: bool, count: usize) -> usize {
        let cursor = self.doc.cursor;
        let starts = self.sentence_starts();
        if backward {
            starts
                .into_iter()
                .rev()
                .filter(|&start| start < cursor)
                .nth(count - 1)
                .unwrap_or(0)
        } else {
            starts
                .into_iter()
                .filter(|&start| start > cursor)
                .nth(count - 1)
                .unwrap_or(self.doc.text.len())
        }
    }

    /// Find the byte offsets of sentence beginnings in document order.
    fn sentence_starts(&self) -> Vec<usize> {
        let text = &self.doc.text;
        let mut starts = vec![0];
        let mut chars = text.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            if matches!(ch, '.' | '!' | '?') {
                while chars
                    .peek()
                    .is_some_and(|&(_, next)| matches!(next, ')' | ']' | '"' | '\''))
                {
                    chars.next();
                }
                if chars.peek().is_none_or(|&(_, next)| next.is_whitespace()) {
                    while chars.peek().is_some_and(|&(_, next)| next.is_whitespace()) {
                        chars.next();
                    }
                    starts.push(chars.peek().map_or(text.len(), |&(i, _)| i));
                }
            } else if ch == '\n' && text[i + 1..].starts_with('\n') {
                starts.push(i + 1);
            }
        }
        starts
    }

    /// Search class and method braces in the requested direction.
    pub(super) fn method_motion(
        &self,
        backward: bool,
        closing: bool,
        count: usize,
    ) -> Option<usize> {
        let cursor = self.doc.cursor;
        let wanted = if closing { '}' } else { '{' };
        let mut depth = 0usize;
        let mut targets = Vec::new();
        for (i, ch) in self.doc.text.char_indices() {
            match ch {
                '{' => {
                    depth += 1;
                    if depth == 1 || ch == wanted && depth == 2 {
                        targets.push(i);
                    }
                }
                '}' => {
                    if depth == 1 || ch == wanted && depth == 2 {
                        targets.push(i);
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
        if backward {
            targets
                .into_iter()
                .rev()
                .filter(|&i| i < cursor)
                .take(count)
                .last()
        } else {
            targets
                .into_iter()
                .filter(|&i| i > cursor)
                .take(count)
                .last()
        }
    }

    /// Build a mark jump to `name`, optionally using its line's first nonblank character.
    pub(super) fn mark_motion(&self, name: char, linewise: bool) -> Option<Motion> {
        let position = self.marks.get(&name).copied().or_else(|| match name {
            '[' => self.operated.as_ref().map(|range| range.start),
            ']' => self
                .operated
                .as_ref()
                .map(|range| range.end.saturating_sub(1)),
            '.' => self.g.last_change(),
            '^' => self.g.insertion,
            '<' | '>' => self.g.visual_mark(name == '>'),
            _ => None,
        })?;
        let position = self
            .doc
            .text
            .floor_char_boundary(position.min(self.doc.text.len()));
        let (end, kind) = if linewise {
            (
                self.first_nonblank(self.doc.location(position).0),
                Kind::Line,
            )
        } else {
            (position, Kind::Exclusive)
        };
        Some(Motion {
            end,
            kind,
            jump: true,
        })
    }
}
