//! Select and operate on named syntax nodes supplied by the host for an exact document version.

use super::{Action, Mode, Operator, Register, ScreenMotionResolver, Vim, motions::Kind};
use crate::i18n;
use std::{cmp::Reverse, ops::Range, path::PathBuf, sync::Arc};

/// Move through the syntax tree from the current character or selection.
#[derive(Clone, Copy)]
pub(super) enum Direction {
    /// Select the containing node, then its successive ancestors.
    Parent,
    /// Return to the previously selected child, or choose the first named child.
    Child,
    /// Select the previous nonempty named sibling.
    Previous,
    /// Select the next nonempty named sibling.
    Next,
}

/// A node for the current source, ordered before all of its descendants.
struct Node {
    /// Half-open UTF-8 range of the entire node.
    bytes: Range<usize>,
    /// Index of the nearest parent with a distinct range.
    parent: Option<usize>,
}

/// A normalized syntax tree for one exact source snapshot and filename.
struct Tree {
    /// Document opening identity and text revision.
    version: (u64, u64),
    /// File path in effect when the host inferred the parser.
    path: Option<PathBuf>,
    /// Parent-first syntax nodes with distinct ranges.
    nodes: Vec<Node>,
}

impl Tree {
    /// Validate `ranges` against `vim` and index their enclosing relationships in document order.
    fn new(vim: &Vim, version: (u64, u64), mut ranges: Vec<Range<usize>>) -> Self {
        let text = &vim.doc.text;
        ranges.retain(|range| {
            range.start < range.end
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
        });
        ranges.sort_unstable_by_key(|range| (range.start, Reverse(range.end)));
        ranges.dedup();
        let mut nodes: Vec<Node> = Vec::with_capacity(ranges.len());
        let mut ancestors: Vec<usize> = Vec::new();
        for bytes in ranges {
            while ancestors
                .last()
                .is_some_and(|&i| nodes[i].bytes.end <= bytes.start)
            {
                ancestors.pop();
            }
            let parent = ancestors.last().copied();
            if parent.is_some_and(|i| nodes[i].bytes.end < bytes.end) {
                continue;
            }
            ancestors.push(nodes.len());
            nodes.push(Node { bytes, parent });
        }
        Self {
            version,
            path: vim.doc.path.clone(),
            nodes,
        }
    }

    /// Find the smallest node containing `range`.
    fn covering(&self, range: &Range<usize>) -> Option<usize> {
        let mut index = self
            .nodes
            .partition_point(|node| node.bytes.start <= range.start)
            .checked_sub(1)?;
        loop {
            let node = &self.nodes[index];
            if range.end <= node.bytes.end {
                return Some(index);
            }
            index = node.parent?;
        }
    }

    /// Find the next index after all descendants of `index`.
    fn subtree_end(&self, index: usize) -> usize {
        self.nodes
            .partition_point(|node| node.bytes.start < self.nodes[index].bytes.end)
    }

    /// Find the closest sibling of `index` in the requested direction, if present.
    fn sibling(&self, index: usize, next: bool) -> Option<usize> {
        let parent = self.nodes[index].parent?;
        if next {
            let index = self.subtree_end(index);
            self.nodes
                .get(index)
                .filter(|node| node.parent == Some(parent))
                .map(|_| index)
        } else {
            let mut index = index - 1;
            while index > parent {
                let ancestor = self.nodes[index].parent?;
                if ancestor == parent {
                    return Some(index);
                }
                index = ancestor;
            }
            None
        }
    }
}

/// Named nodes visited when expanding the current selection.
#[derive(Default)]
struct History {
    /// Node currently displayed, if the remembered path is still in use.
    current: Option<usize>,
    /// Children to return to in reverse order of expansion.
    children: Vec<usize>,
}

impl History {
    /// Advance up to `count` times through `tree`, starting at the visible `range`.
    fn advance(
        &mut self,
        tree: &Tree,
        range: Range<usize>,
        direction: Direction,
        count: usize,
    ) -> Option<Range<usize>> {
        let siblings = matches!(direction, Direction::Previous | Direction::Next);
        let covering = tree.covering(&range);
        let mut index = covering.unwrap_or(0);
        let exact = range == tree.nodes[index].bytes;
        if !exact || self.current != Some(index) {
            self.children.clear();
        }
        if siblings && covering.is_none() {
            return None;
        }
        let mut changed = !siblings;
        let mut steps = count;
        if !exact && !siblings {
            if covering.is_some() && matches!(direction, Direction::Child) {
                let first = index + 1;
                if let Some(relative) = tree.nodes[first..tree.subtree_end(index)]
                    .iter()
                    .position(|node| range.start <= node.bytes.start && node.bytes.end <= range.end)
                {
                    index = first + relative;
                }
            }
            steps -= 1;
        }
        for _ in 0..steps {
            let node = &tree.nodes[index];
            let next = match direction {
                Direction::Parent => node.parent.inspect(|_| self.children.push(index)),
                Direction::Child => self.children.pop().or_else(|| {
                    tree.nodes
                        .get(index + 1)
                        .filter(|node| node.parent == Some(index))
                        .map(|_| index + 1)
                }),
                Direction::Previous | Direction::Next => {
                    self.children.clear();
                    tree.sibling(index, matches!(direction, Direction::Next))
                }
            };
            let Some(next) = next else { break };
            index = next;
            changed = true;
        }
        self.current = Some(index);
        changed.then(|| tree.nodes[index].bytes.clone())
    }
}

/// An operation waiting for the syntax tree of an exact editor state.
struct Request {
    /// Monotonic identity checked by the host before delivering a reply.
    serial: u64,
    /// Document opening identity and text revision when requested.
    version: (u64, u64),
    /// File path in effect when the syntax lookup began.
    path: Option<PathBuf>,
    /// Editor mode, selection anchor and caret when requested.
    selection: (Mode, usize, usize),
    /// Direction of the requested navigation.
    direction: Direction,
    /// Number of node transitions requested.
    count: usize,
    /// Deferred operator, if this syntax selection was requested from operator-pending mode.
    operator: Option<Operator>,
    /// Explicit register for the deferred operator.
    register: Option<char>,
}

/// Tree cache, expansion path and outstanding request owned by one Vim buffer.
#[derive(Default)]
pub(super) struct State {
    /// Cached nodes for the most recently parsed version and path.
    tree: Option<Tree>,
    /// Previously visited children of the current selection.
    history: History,
    /// Selection awaiting a parser reply from the host.
    request: Option<Request>,
    /// Last serial issued to a parser request for this buffer.
    serial: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::vim::replay::Stroke;

    /// C source containing both single-character nodes and nested expressions.
    const CODE: &str = "int square(int x) { return x * x; }";

    /// Return the named C node ranges used by the selection tests.
    fn ranges() -> Arc<[Range<usize>]> {
        Arc::from([
            0..35,
            0..3,
            4..17,
            4..10,
            10..17,
            11..16,
            11..14,
            15..16,
            18..35,
            20..33,
            27..32,
            27..28,
            31..32,
        ])
    }

    /// Open `CODE` at the first operand in the return statement.
    fn fixture() -> (Vim, Register) {
        let mut vim = Vim::new(Document::new(CODE));
        vim.set_cursor(27);
        (vim, Register::default())
    }

    /// Select two ancestors, step back through both remembered children and move to a sibling.
    #[test]
    fn syntax_selection_expands_contracts_and_remembers_children() {
        let (mut vim, mut register) = fixture();
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("v2an", &mut register), Action::SyntaxSelection);
        assert!(vim.syntax_selection_request().is_some());
        assert_eq!(
            vim.apply_syntax_selection(version, ranges(), &mut register),
            Action::None
        );
        assert_eq!(vim.selection(), Some(20..33));
        vim.text("in", &mut register);
        assert_eq!(vim.selection(), Some(27..32));
        vim.text("in", &mut register);
        assert_eq!(vim.selection(), Some(27..28));
        vim.text("]n", &mut register);
        assert_eq!(vim.selection(), Some(31..32));
        vim.text("[n", &mut register);
        assert_eq!(vim.selection(), Some(27..28));
        assert_eq!(vim.doc.text, CODE);
    }

    /// The child command picks a named child contained by a partial range.
    #[test]
    fn syntax_selection_contracts_a_partial_backwards_range() {
        let (mut vim, mut register) = fixture();
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("vlloan", &mut register), Action::SyntaxSelection);
        vim.apply_syntax_selection(version, ranges(), &mut register);
        assert_eq!(vim.selection(), Some(27..32));
        assert_eq!((vim.anchor, vim.doc.cursor), (31, 27));
        vim.text("in", &mut register);
        assert_eq!(vim.selection(), Some(27..28));
        vim.text("lloin", &mut register);
        assert_eq!(vim.selection(), Some(27..28));
    }

    /// A deferred operator retains its count, register and undo checkpoint.
    #[test]
    fn syntax_object_can_apply_a_counted_operator() {
        let (mut vim, mut register) = fixture();
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("\"a2dan", &mut register), Action::SyntaxSelection);
        assert_eq!(vim.doc.text, CODE);
        vim.apply_syntax_selection(version, ranges(), &mut register);
        assert_eq!(vim.doc.text, "int square(int x) {  }");
        assert_eq!(register.read(Some('a')).text, "return x * x;");
        assert_eq!(vim.mode, Mode::Normal);
        vim.text("u", &mut register);
        assert_eq!(vim.doc.text, CODE);
    }

    /// Later input cancels a pending request and a path change invalidates a matching revision.
    #[test]
    fn syntax_selection_ignores_stale_parser_results() {
        let (mut vim, mut register) = fixture();
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("dan", &mut register), Action::SyntaxSelection);
        vim.text("l", &mut register);
        assert_eq!(vim.syntax_selection_request(), None);
        vim.apply_syntax_selection(version, ranges(), &mut register);
        assert_eq!(vim.doc.text, CODE);
        assert_eq!(vim.text("dan", &mut register), Action::SyntaxSelection);
        vim.doc.path = Some(PathBuf::from("square.txt"));
        vim.apply_syntax_selection(version, ranges(), &mut register);
        assert_eq!(vim.doc.text, CODE);
    }

    /// An unavailable parser is retried on the next command at the same revision.
    #[test]
    fn syntax_selection_retries_empty_parser_results() {
        let (mut vim, mut register) = fixture();
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("van", &mut register), Action::SyntaxSelection);
        vim.apply_syntax_selection(version, Arc::from([]), &mut register);
        assert_eq!(vim.selection(), Some(27..28));
        assert!(!vim.message.is_empty());
        assert_eq!(vim.text("an", &mut register), Action::SyntaxSelection);
        vim.apply_syntax_selection(version, ranges(), &mut register);
        assert_eq!(vim.selection(), Some(27..32));
    }

    /// Dot repeat runs the Insert strokes after reparsing the changed source.
    #[test]
    fn syntax_change_replays_insert_after_parser_result() {
        let mut vim = Vim::new(Document::new("abc def"));
        let mut register = Register::default();
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("can", &mut register), Action::SyntaxSelection);
        vim.apply_syntax_selection(version, Arc::from([0..7, 0..3, 4..7]), &mut register);
        assert_eq!(vim.mode, Mode::Insert);
        vim.commit_text("X", &mut register);
        vim.key("Escape", &mut register);
        vim.text("w.", &mut register);
        assert!(vim.syntax_selection_request().is_some());
        let version = (vim.doc.opening, vim.doc.revision);
        vim.apply_syntax_selection(version, Arc::from([0..5, 0..1, 2..5]), &mut register);
        assert_eq!(vim.doc.text, "X X");
        assert_eq!(vim.mode, Mode::Normal);
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "X def");
    }

    /// Counted nested macro playback resumes its tail and shares a single undo checkpoint.
    #[test]
    fn syntax_object_resumes_counted_nested_macros() {
        let mut vim = Vim::new(Document::new("abc def ghi"));
        let mut register = Register::default();
        for (name, keys) in [
            ('a', &["c", "a", "n", "X", "Escape", "w"][..]),
            ('b', &["@", "a"]),
        ] {
            register.recording = Some((
                name,
                keys.iter().map(|key| Stroke::Key((*key).into())).collect(),
            ));
            register.stop_recording();
        }
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(vim.text("2@b", &mut register), Action::SyntaxSelection);
        assert_eq!(
            vim.apply_syntax_selection(
                version,
                Arc::from([0..11, 0..3, 4..7, 8..11]),
                &mut register
            ),
            Action::SyntaxSelection
        );
        assert_eq!(vim.doc.text, "X def ghi");
        let version = (vim.doc.opening, vim.doc.revision);
        assert_eq!(
            vim.apply_syntax_selection(version, Arc::from([0..9, 0..1, 2..5, 6..9]), &mut register),
            Action::None
        );
        assert_eq!(vim.doc.text, "X X ghi");
        assert_eq!(vim.doc.cursor, 4);
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "abc def ghi");
    }
}

impl State {
    /// Invalidate the active request and return whether an operation was pending.
    pub(super) fn cancel_request(&mut self) -> bool {
        self.request.take().is_some()
    }
}

impl Vim {
    /// Identify the syntax selection waiting for a host parser reply, if any.
    pub fn syntax_selection_request(&self) -> Option<u64> {
        self.syntax_selection
            .request
            .as_ref()
            .map(|request| request.serial)
    }

    /// Start or apply a counted syntax selection, optionally passing its byte range to `operator`.
    pub(super) fn select_syntax(
        &mut self,
        direction: Direction,
        count: usize,
        operator: Option<Operator>,
        register: &mut Register,
    ) -> Action {
        let version = (self.doc.opening, self.doc.revision);
        if self
            .syntax_selection
            .tree
            .as_ref()
            .is_some_and(|tree| tree.version == version && tree.path == self.doc.path)
        {
            self.select_cached_syntax(direction, count, operator, register);
            return Action::None;
        }
        self.syntax_selection.serial = self.syntax_selection.serial.wrapping_add(1);
        self.syntax_selection.request = Some(Request {
            serial: self.syntax_selection.serial,
            version,
            path: self.doc.path.clone(),
            selection: (self.mode, self.anchor, self.doc.cursor),
            direction,
            count,
            operator,
            register: self.selected_register,
        });
        Action::SyntaxSelection
    }

    /// Complete the still-current syntax operation with `nodes` for `version` using `register`.
    /// An empty list represents a document with no available syntax parser or nodes.
    pub fn apply_syntax_selection(
        &mut self,
        version: (u64, u64),
        nodes: Arc<[Range<usize>]>,
        register: &mut Register,
    ) -> Action {
        self.apply_syntax_selection_with_screen_rows(version, nodes, register, &mut |_, _, _, _| {
            None
        })
    }

    /// Complete the syntax operation and resolve any screen motions in suspended playback.
    pub fn apply_syntax_selection_with_screen_rows(
        &mut self,
        version: (u64, u64),
        nodes: Arc<[Range<usize>]>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let Some(request) = self.syntax_selection.request.take() else {
            return Action::None;
        };
        if request.version != version
            || version != (self.doc.opening, self.doc.revision)
            || request.path != self.doc.path
            || request.selection != (self.mode, self.anchor, self.doc.cursor)
        {
            self.stop_deferred_replay();
            return Action::None;
        }
        let tree = Tree::new(self, version, nodes.to_vec());
        let available = !tree.nodes.is_empty();
        self.syntax_selection.tree = available.then_some(tree);
        self.syntax_selection.history = History::default();
        let insert_resume = self.insert_resume.take();
        if available {
            self.selected_register = request.register;
            self.select_cached_syntax(request.direction, request.count, request.operator, register);
            self.selected_register = None;
        } else {
            self.message = i18n::text("vim_hint.syntax_unavailable").into();
            self.stop_deferred_replay();
        }
        let (mode, anchor, cursor) = request.selection;
        self.settle_fold_cursor(cursor, version.1);
        self.track_g_input(mode, anchor, cursor, version.1, true);
        if insert_resume.is_none() {
            self.finish_replay_change(&Action::None);
        }
        let action = self.resume_deferred_replay(register, resolve_screen);
        if super::replay::defers_replay(&action) {
            self.insert_resume = insert_resume.or(self.insert_resume.take());
        } else if let Some(resume) = insert_resume {
            self.resume_insert(resume);
            self.finish_replay_change(&action);
        }
        action
    }

    /// Apply a syntax operation with the tree already indexed for this version.
    fn select_cached_syntax(
        &mut self,
        direction: Direction,
        count: usize,
        operator: Option<Operator>,
        register: &mut Register,
    ) {
        let state = &mut self.syntax_selection;
        let tree = state.tree.as_ref().expect("syntax tree checked");
        let reverse = self.mode.is_visual() && self.anchor > self.doc.cursor;
        // Neovim uses the literal visual endpoints in character, line and block modes.
        let other = if self.mode.is_visual() {
            self.anchor
        } else {
            self.doc.cursor
        };
        let range = other.min(self.doc.cursor)..self.doc.next(other.max(self.doc.cursor));
        let Some(range) = state.history.advance(tree, range, direction, count.max(1)) else {
            return;
        };
        if let Some(operator) = operator {
            self.operate_object(operator, range, Kind::Exclusive, register);
        } else {
            let last = self.doc.previous(range.end).max(range.start);
            let (anchor, caret) = if reverse {
                (last, range.start)
            } else {
                (range.start, last)
            };
            self.anchor = anchor;
            // Keep the selected endpoint visible without changing the fold's durable state.
            self.doc
                .set_search_fold_row(Some(self.doc.location(caret).0));
            self.set_cursor(caret);
            if self.doc.text.as_bytes().get(caret) == Some(&b'\n') {
                self.doc.cursor = caret;
                self.desired_column = Some(usize::MAX);
            }
        }
    }
}
