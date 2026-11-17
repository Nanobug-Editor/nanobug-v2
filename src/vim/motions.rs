//! Grapheme motions and logical viewport commands used by Normal, Visual and operators.

use super::Vim;
use unicode_segmentation::UnicodeSegmentation;

/// Range semantics of a motion when used after an operator.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// The farther endpoint is excluded.
    Exclusive,
    /// Both endpoints are included.
    Inclusive,
    /// Every line intersecting the motion is included.
    Line,
}

/// Destination and operator semantics computed for one motion.
pub(super) struct Motion {
    /// UTF-8 byte destination.
    pub end: usize,
    /// Shape of the resulting operator range.
    pub kind: Kind,
    /// Whether direct movement records its origin in the jumplist.
    pub jump: bool,
}

impl Motion {
    /// Construct a non-jumping motion to `end` with this `kind`.
    pub fn new(end: usize, kind: Kind) -> Self {
        Self {
            end,
            kind,
            jump: false,
        }
    }
}

/// Latest f, F, t or T command and its literal target.
pub(super) struct Find {
    /// Find direction and whether the destination stops short.
    pub key: char,
    /// A single Unicode grapheme to find on the current line.
    pub text: String,
}

/// Horizontal viewport placement requested by a modal command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HorizontalScroll {
    /// Use this zero-based column as the left edge.
    Position(usize),
    /// Align the start of the caret's glyph to the left edge.
    CursorLeft,
    /// Align the end of the caret's glyph to the right edge.
    CursorRight,
}

/// Target within the measured screen lines of a wrapped buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenMotion {
    /// Move to the corresponding column of a preceding display row.
    Up,
    /// Move to the corresponding column of a following display row.
    Down,
    /// Move to the left edge of the current display row.
    Start,
    /// Move to the first nonblank character of the current display row.
    FirstNonblank,
    /// Move halfway across the current display row.
    Middle,
    /// Move to the last available character in the counted display row.
    End,
    /// Move to the last nonblank character in the counted display row.
    LastNonblank,
}

/// Resolve a screen-line motion with the editor's current measured wrapping.
/// The arguments are the Vim state, requested target, count and preferred horizontal position.
/// Return the document byte and preferred position, or `None` for unwrapped movement.
pub type ScreenMotionResolver<'a> =
    dyn FnMut(&Vim, ScreenMotion, usize, Option<usize>) -> Option<(usize, usize)> + 'a;

impl Vim {
    /// Keep command-mode carets on a character in a logical line.
    pub fn clamp_normal(&mut self) {
        if self.mode.is_insert() {
            return;
        }
        let row = self
            .doc
            .location(self.doc.cursor)
            .0
            .min(self.line_count() - 1);
        let range = self.doc.line_range(row);
        self.doc.cursor = self.doc.cursor.min(range.end);
        if self.doc.cursor == range.end
            && range.end > range.start
            && !(self.mode.is_visual() && self.desired_column == Some(usize::MAX))
        {
            self.doc.cursor = self.doc.previous(range.end);
        }
    }

    /// Place the caret at byte `offset`, resetting the preferred display column.
    pub fn set_cursor(&mut self, offset: usize) {
        if self.doc.cursor != offset {
            self.cancel_syntax_request();
        }
        self.folded_closing_caret = None;
        self.screen_column = None;
        self.doc.cursor = self
            .doc
            .text
            .floor_char_boundary(offset.min(self.doc.text.len()));
        self.desired_column = None;
        self.clamp_normal();
        self.doc
            .update_search_fold_row(self.doc.location(self.doc.cursor).0);
    }

    /// Jump to a byte offset in the current document, preserving the source in the jumplist.
    pub fn jump_to(&mut self, offset: usize) {
        if self.doc.cursor != offset {
            self.record_jump();
        }
        self.place_jump(offset);
    }

    /// Move the viewport-following caret to `row` while retaining its preferred column.
    pub fn move_to_row(&mut self, row: usize) {
        let previous = self.doc.cursor;
        self.folded_closing_caret = None;
        self.screen_column = None;
        self.doc.cursor = self.row_target(row);
        self.clamp_normal();
        if self.doc.cursor != previous {
            self.cancel_syntax_request();
        }
        self.settle_fold_cursor(previous, self.doc.revision);
    }

    /// Resolve a vertical `row` using the retained display column.
    pub(super) fn row_target(&mut self, row: usize) -> usize {
        let column = *self
            .desired_column
            .get_or_insert_with(|| self.doc.location(self.doc.cursor).1);
        self.doc.offset(row.min(self.line_count() - 1), column)
    }

    /// Move to the resolved `motion`, recording jumps where appropriate.
    pub(super) fn apply_motion(&mut self, motion: Motion) {
        if motion.jump && self.doc.cursor != motion.end {
            self.record_jump();
        }
        self.doc.cursor = motion.end;
        self.folded_closing_caret = None;
        self.clamp_normal();
    }

    /// Resolve an operator motion for `key` and `count` in document rows.
    pub(super) fn motion(&mut self, key: &str, count: Option<usize>) -> Option<Motion> {
        self.motion_with_rows(key, count, false)
    }

    /// Resolve `key` and `count` for cursor movement, skipping folded rows on vertical steps.
    pub(super) fn cursor_motion(&mut self, key: &str, count: Option<usize>) -> Option<Motion> {
        self.motion_with_rows(key, count, true)
    }

    /// Resolve `key` and its optional `count` using visible rows when requested.
    fn motion_with_rows(
        &mut self,
        key: &str,
        count: Option<usize>,
        visible: bool,
    ) -> Option<Motion> {
        let n = count.unwrap_or(1);
        let at = self.doc.cursor;
        let row = self.doc.location(at).0;
        let last = self.line_count() - 1;
        let line = self.doc.line_range(row);
        let mut kind = Kind::Exclusive;
        let mut jump = false;
        let end = match key {
            "h" | "Left" | "Backspace" | "Ctrl+h" => {
                let mut p = at;
                for _ in 0..n {
                    if p == line.start {
                        break;
                    }
                    p = self.doc.previous(p);
                }
                p
            }
            "l" | "Right" | " " => {
                let mut p = at;
                for _ in 0..n {
                    if p == line.end {
                        break;
                    }
                    p = self.doc.next(p).min(line.end);
                }
                p
            }
            "j" | "Down" | "Ctrl+j" | "Ctrl+n" | "k" | "Up" | "Ctrl+p" => {
                let down = matches!(key, "j" | "Down" | "Ctrl+j" | "Ctrl+n");
                let (first, limit) = if visible {
                    (self.doc.display_row(row), self.doc.display_rows() - 1)
                } else {
                    (row, last)
                };
                let target = if down {
                    first.saturating_add(n).min(limit)
                } else {
                    first.saturating_sub(n)
                };
                let target = if visible {
                    self.doc.document_row(target)
                } else {
                    target
                };
                if target == row {
                    return None;
                }
                return Some(Motion::new(self.row_target(target), Kind::Line));
            }
            "0" | "Home" => line.start,
            "^" => self.first_nonblank(row),
            "$" | "End" => {
                if n > 1 && row == last {
                    return None;
                }
                let target = row.saturating_add(n - 1).min(last);
                let end = self.doc.line_range(target);
                self.desired_column = Some(usize::MAX);
                if self.mode.is_insert() || self.mode.is_visual() || end.is_empty() {
                    return Some(Motion::new(end.end, Kind::Exclusive));
                }
                return Some(Motion::new(self.doc.previous(end.end), Kind::Inclusive));
            }
            "g_" => {
                kind = Kind::Inclusive;
                let target = row.saturating_add(n - 1).min(last);
                let range = self.doc.line_range(target);
                let end = range.start + self.doc.text[range].trim_end_matches([' ', '\t']).len();
                self.doc.previous(end).max(self.doc.starts[target])
            }
            "w" | "W" | "b" | "B" | "e" | "E" | "Shift+Left" | "Shift+Right" | "Ctrl+left"
            | "Ctrl+right" | "Ctrl+Left" | "Ctrl+Right" => {
                let mut p = at;
                let big = matches!(key, "W" | "B" | "E");
                for _ in 0..n {
                    let next = match key {
                        "w" | "W" | "Shift+Right" | "Ctrl+right" | "Ctrl+Right" => {
                            self.word_forward(p, big)
                        }
                        "b" | "B" | "Shift+Left" | "Ctrl+left" | "Ctrl+Left" => {
                            self.word_backward(p, big)
                        }
                        _ => {
                            kind = Kind::Inclusive;
                            self.word_end(p, big)
                        }
                    };
                    if p == next {
                        break;
                    }
                    p = next;
                }
                p
            }
            "G" | "gg" | "Ctrl+Home" | "Ctrl+home" | "Ctrl+End" | "Ctrl+end" => {
                let first = matches!(key, "gg" | "Ctrl+Home" | "Ctrl+home");
                let target = count.map_or(if first { 0 } else { last }, |n| (n - 1).min(last));
                return Some(self.line_jump(target));
            }
            "-" | "+" | "_" | "Enter" | "Ctrl+m" => {
                kind = Kind::Line;
                let target = match key {
                    "-" => row.saturating_sub(n),
                    "_" => row.saturating_add(n - 1).min(last),
                    _ => row.saturating_add(n).min(last),
                };
                self.first_nonblank(target)
            }
            "|" => {
                self.desired_column = Some(n - 1);
                return Some(Motion::new(self.doc.offset(row, n - 1), Kind::Exclusive));
            }
            "%" if count.is_some() => {
                if n > 100 {
                    return None;
                }
                return Some(self.line_jump((self.line_count() * n).div_ceil(100) - 1));
            }
            "%" => {
                kind = Kind::Inclusive;
                jump = true;
                let bracket = self.doc.text[at..line.end]
                    .char_indices()
                    .find(|(_, ch)| "()[]{}".contains(*ch))
                    .map(|(i, _)| at + i)?;
                self.paired(bracket)?
            }
            ";" | "," => return self.repeat_find(key == ",", n),
            "(" | ")" => {
                jump = true;
                self.sentence_motion(key == "(", n)
            }
            "{" | "}" => {
                jump = true;
                self.paragraph_motion(key == "{", n)
            }
            "[m" | "]m" | "[M" | "]M" => {
                jump = true;
                self.method_motion(key.starts_with('['), key.ends_with('M'), n)?
            }
            "H" | "M" | "L" => {
                let last = self.doc.display_rows() - 1;
                let top = self.view.top.min(last);
                let bottom = top.saturating_add(self.view.rows - 1).min(last);
                let target = match key {
                    "H" => top.saturating_add(n - 1),
                    "L" => bottom.saturating_sub(n - 1),
                    _ => top + (bottom - top) / 2,
                };
                let (first, last) = self.viewport_limits(top);
                return Some(self.line_jump(self.doc.document_row(target.clamp(first, last))));
            }
            _ => return None,
        };
        self.desired_column = None;
        Some(Motion { end, kind, jump })
    }

    /// Build a linewise jump to `row` while retaining the preferred column.
    fn line_jump(&mut self, row: usize) -> Motion {
        Motion {
            end: self.row_target(row),
            kind: Kind::Line,
            jump: true,
        }
    }

    /// Byte of the first nonblank character on `row`.
    pub(super) fn first_nonblank(&self, row: usize) -> usize {
        let line = self.doc.line(row);
        self.doc.starts[row] + line.len() - line.trim_start_matches([' ', '\t']).len()
    }

    /// Classify the grapheme at `at` as whitespace, keyword or punctuation.
    pub(super) fn class(&self, at: usize, big: bool) -> u8 {
        match self.doc.text[at..].chars().next() {
            None => 0,
            Some(c) if c.is_whitespace() => 0,
            Some(c) if big || c.is_alphanumeric() || c == '_' => 1,
            _ => 2,
        }
    }

    /// Beginning of the next word after `at`; a WORD only splits on whitespace.
    pub(super) fn word_forward(&self, mut at: usize, big: bool) -> usize {
        let class = self.class(at, big);
        while at < self.doc.text.len() && self.class(at, big) == class {
            at = self.doc.next(at);
            if self.empty_line_start(at) {
                return at;
            }
        }
        while at < self.doc.text.len() && self.class(at, big) == 0 {
            at = self.doc.next(at);
            if self.empty_line_start(at) {
                break;
            }
        }
        at
    }

    /// Beginning of the previous word before `at`.
    pub(super) fn word_backward(&self, at: usize, big: bool) -> usize {
        let mut at = self.doc.previous(at);
        while at > 0 && self.class(at, big) == 0 && !self.empty_line_start(at) {
            at = self.doc.previous(at);
        }
        let class = self.class(at, big);
        while class != 0 && at > 0 && self.class(self.doc.previous(at), big) == class {
            at = self.doc.previous(at);
        }
        at
    }

    /// Final grapheme of the next or current word after `at`.
    pub(super) fn word_end(&self, at: usize, big: bool) -> usize {
        let mut end = self.doc.next(at);
        while end < self.doc.text.len() && self.class(end, big) == 0 {
            end = self.doc.next(end);
        }
        if end == self.doc.text.len() {
            let line = self.doc.line_range(self.line_count() - 1);
            return at.max(self.doc.previous(line.end).max(line.start));
        }
        let class = self.class(end, big);
        while self.doc.next(end) < self.doc.text.len()
            && self.class(self.doc.next(end), big) == class
        {
            end = self.doc.next(end);
        }
        end
    }

    /// Whether `at` is an empty line that counts as a word boundary.
    fn empty_line_start(&self, at: usize) -> bool {
        self.doc.text.as_bytes().get(at) == Some(&b'\n')
            && (at == 0 || self.doc.text.as_bytes()[at - 1] == b'\n')
    }

    /// Search for `text` with find command `key` and return the `count`-th match.
    pub(super) fn find_motion(&mut self, key: char, text: &str, count: usize) -> Option<Motion> {
        if text.graphemes(true).count() != 1 || text == "\n" {
            return None;
        }
        let result = self.find_on_line(key, text, count, false);
        self.last_find = Some(Find {
            key,
            text: text.into(),
        });
        result
    }

    /// Repeat the saved find, optionally in the `reverse` direction.
    fn repeat_find(&self, reverse: bool, count: usize) -> Option<Motion> {
        let find = self.last_find.as_ref()?;
        let key = if reverse {
            if find.key.is_ascii_uppercase() {
                find.key.to_ascii_lowercase()
            } else {
                find.key.to_ascii_uppercase()
            }
        } else {
            find.key
        };
        self.find_on_line(key, &find.text, count, true)
    }

    /// Scan the current line for a literal grapheme, honoring find/till inclusivity.
    fn find_on_line(&self, key: char, text: &str, count: usize, repeat: bool) -> Option<Motion> {
        let at = self.doc.cursor;
        let range = self.doc.line_range(self.doc.location(at).0);
        let backward = key.is_ascii_uppercase();
        let till = matches!(key, 't' | 'T');
        let target = |p| {
            if !till {
                p
            } else if backward {
                self.doc.next(p)
            } else {
                self.doc.previous(p)
            }
        };
        let matched =
            |(i, ch): &(usize, &str)| *ch == text && (!repeat || target(range.start + *i) != at);
        let chars = self.doc.text[range.clone()].grapheme_indices(true);
        let (byte, _) = if backward {
            chars
                .rev()
                .filter(|(i, _)| range.start + *i < at)
                .filter(matched)
                .nth(count - 1)?
        } else {
            chars
                .filter(|(i, _)| range.start + *i > at)
                .filter(matched)
                .nth(count - 1)?
        };
        Some(Motion::new(
            target(range.start + byte),
            if backward {
                Kind::Exclusive
            } else {
                Kind::Inclusive
            },
        ))
    }

    /// Set the logical left offset and visible `columns` supplied by the viewport.
    pub fn set_view_width(&mut self, left: usize, columns: usize) {
        self.view.left = left;
        self.view.columns = columns.max(1);
        self.view.horizontal = None;
    }

    /// Consume the requested horizontal placement after the completed command.
    pub fn take_horizontal_scroll(&mut self) -> Option<HorizontalScroll> {
        self.view.horizontal.take()
    }

    /// Restrict `available` viewport rows to the last explicitly requested height.
    pub fn view_height(&self, available: usize) -> usize {
        self.view
            .height_limit
            .map_or(available, |rows| rows.min(available))
    }

    /// Use `rows` as the logical viewport height, bounded by the host's available area.
    pub(super) fn resize_view_height(&mut self, rows: usize) {
        self.view.height_limit = Some(rows.max(1).min(self.view.available_rows));
        self.view.rows = self.view_height(self.view.available_rows);
        self.view.half_page = self.view.half_page.map(|half| half.min(self.view.rows));
    }

    /// Scroll visible rows or columns for `key`, with a line or repeat `count` when supplied.
    pub(super) fn scroll(&mut self, key: &str, count: Option<usize>) -> bool {
        let height = self.view.rows;
        let doc_row = self.doc.location(self.doc.cursor).0;
        let row = self.doc.display_row(doc_row);
        let last = self.doc.display_rows() - 1;
        let top = self.view.top.min(last);
        let full_top = self.doc.display_rows().saturating_sub(height);
        let margin = Self::scroll_margin(height);
        let n = count.unwrap_or(1);
        let (destination, caret) = match key {
            "zz" | "zt" | "zb" | "zEnter" | "z." | "z-" | "z+" | "z^" => {
                let last_line = self.line_count() - 1;
                let caret = if let Some(n) = count {
                    let target = n.saturating_sub(1).min(last_line);
                    if target != doc_row {
                        self.record_jump();
                    }
                    self.doc.display_row(target)
                } else {
                    let last = self.doc.display_row(last_line);
                    match key {
                        "z+" => top.saturating_add(height).min(last),
                        "z^" => top.saturating_sub(1),
                        _ => row,
                    }
                };
                let last = self.doc.display_rows() - 1;
                let bottom_offset = |at: usize| height - 1 - margin.min(last - at);
                let caret = if key == "z^" && count.is_some() {
                    caret.saturating_sub(bottom_offset(caret))
                } else {
                    caret
                };
                let offset = match key {
                    "zt" | "zEnter" | "z+" => margin,
                    "zb" | "z-" | "z^" => bottom_offset(caret),
                    _ => (height - 1) / 2,
                };
                (caret.saturating_sub(offset), caret)
            }
            "zh" | "zl" | "zH" | "zL" | "zLeft" | "zRight" | "zs" | "ze" => {
                self.scroll_columns(key, n);
                return true;
            }
            "Ctrl+e" | "Ctrl+y" => {
                let next = if key == "Ctrl+e" {
                    top.saturating_add(n).min(last)
                } else {
                    top.saturating_sub(n)
                };
                let (first, last) = self.viewport_limits(next);
                (next, row.clamp(first, last))
            }
            "Ctrl+u" | "Ctrl+d" => {
                if let Some(n) = count {
                    self.view.half_page = Some(n.min(height));
                }
                let amount = self.view.half_page.unwrap_or((height / 2).max(1));
                if key == "Ctrl+d" {
                    (
                        top.saturating_add(amount).min(full_top.max(top)),
                        row.saturating_add(amount),
                    )
                } else {
                    (top.saturating_sub(amount), row.saturating_sub(amount))
                }
            }
            "Ctrl+f" | "Ctrl+b" | "PageUp" | "PageDown" => Self::page_scroll(
                top,
                height,
                last,
                row,
                n,
                matches!(key, "Ctrl+f" | "PageDown"),
            ),
            _ => return false,
        };
        let caret = self
            .doc
            .document_row(caret.min(self.doc.display_rows() - 1));
        if matches!(key, "zEnter" | "z." | "z-" | "z+" | "z^") {
            self.set_cursor(self.first_nonblank(caret));
        } else if caret != doc_row {
            self.move_to_row(caret);
        }
        self.view.top = destination;
        self.view.changed = true;
        true
    }

    /// Advance `count` pages from `top` toward the `last` visible row when `down` is true,
    /// otherwise toward the start. Return the new top and caret from `row` in a `height`-row view.
    pub fn page_scroll(
        top: usize,
        height: usize,
        last: usize,
        row: usize,
        count: usize,
        down: bool,
    ) -> (usize, usize) {
        let height = height.max(1);
        let page = if height <= 2 { height } else { height - 2 };
        let next = if down {
            let full_top = last.saturating_add(1).saturating_sub(height);
            let mut next = top;
            for _ in 0..count {
                next = if next >= full_top {
                    last
                } else {
                    next.saturating_add(page).min(last)
                };
                if next == last {
                    break;
                }
            }
            next
        } else {
            top.saturating_sub(count.saturating_mul(page))
        };
        let (first, bottom) = Self::screen_limits(next, height, last);
        let caret = if next == top {
            row
        } else if down {
            first
        } else {
            bottom
        };
        (next, caret)
    }

    /// Handle a nowrap horizontal command `key`, repeated `count` cells or half pages.
    fn scroll_columns(&mut self, key: &str, count: usize) {
        let (row, column) = self.doc.location(self.doc.cursor);
        let width = self.view.columns;
        let cell_width = self
            .doc
            .display_width(
                &self.doc.text[self.doc.cursor
                    ..self
                        .doc
                        .next(self.doc.cursor)
                        .min(self.doc.line_range(row).end)],
                column,
            )
            .max(1);
        let (left, placement) = match key {
            "zs" | "ze" if self.doc.folded_at(row) => (0, HorizontalScroll::Position(0)),
            "zs" => (column, HorizontalScroll::CursorLeft),
            "ze" => (
                (column + cell_width).saturating_sub(width),
                HorizontalScroll::CursorRight,
            ),
            _ => {
                let cells = if matches!(key, "zH" | "zL") {
                    count.saturating_mul(width / 2)
                } else {
                    count
                };
                let left = if matches!(key, "zh" | "zH" | "zLeft") {
                    self.view.left.saturating_sub(cells)
                } else {
                    self.view.left.saturating_add(cells)
                };
                if left != self.view.left {
                    let end = left.saturating_add(width);
                    if column < left {
                        let at = self.doc.offset(row, left);
                        let (_, start) = self.doc.location(at);
                        self.set_cursor(if start < left {
                            self.doc.next(at).min(self.doc.line_range(row).end)
                        } else {
                            at
                        });
                    } else if column >= end || column + cell_width > end {
                        let at = self.doc.offset(row, end - 1);
                        let (_, start) = self.doc.location(at);
                        let next = self.doc.next(at).min(self.doc.line_range(row).end);
                        let end_of_cell =
                            start + self.doc.display_width(&self.doc.text[at..next], start);
                        self.set_cursor(if end_of_cell > end {
                            self.doc.previous(at)
                        } else {
                            at
                        });
                    }
                }
                let left = left.min(self.doc.location(self.doc.cursor).1);
                (left, HorizontalScroll::Position(left))
            }
        };
        self.view.left = left;
        self.view.horizontal = Some(placement);
    }

    /// Complete rows of padding requested at each scrollable screen edge.
    fn scroll_margin(height: usize) -> usize {
        3.min((height - 1) / 2)
    }

    /// Legal interval of display rows when the viewport starts at `top`.
    fn viewport_limits(&self, top: usize) -> (usize, usize) {
        Self::screen_limits(top, self.view.rows, self.doc.display_rows() - 1)
    }

    /// Keep the caret within the screen margins for a `height`-row viewport at `top` through `last_row`.
    fn screen_limits(top: usize, height: usize, last_row: usize) -> (usize, usize) {
        let bottom = top.saturating_add(height - 1).min(last_row);
        let margin = Self::scroll_margin(height);
        let first = if top == 0 { top } else { top + margin }.min(bottom);
        let last = if bottom == last_row {
            bottom
        } else {
            bottom - margin
        }
        .max(first);
        (first, last)
    }
}
