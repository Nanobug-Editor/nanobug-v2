//! Preserve the highlight of each emitted Ex message segment.

use super::{Action, HighlightStyle, Register, Vim};
use std::ops::Range;

pub(super) const NAMES: &[&str] = &["echohl"];

/// An echoed UTF-8 text range and the style that was active when it was emitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExMessageSpan {
    pub range: Range<usize>,
    pub style: HighlightStyle,
}

/// Message text and its sorted highlight spans, which are separate from the output stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExMessageOutput {
    pub text: String,
    pub spans: Vec<ExMessageSpan>,
}

impl ExMessageOutput {
    /// Form one history entry with the same style across its complete text.
    pub(super) fn with_style(text: &str, style: Option<HighlightStyle>) -> Self {
        let mut output = Self::default();
        output.push(text, style);
        output
    }

    /// Append a fragment, keeping ordinary characters outside any highlight span.
    pub(super) fn push(&mut self, text: &str, style: Option<HighlightStyle>) {
        let start = self.text.len();
        self.text.push_str(text);
        if !text.is_empty()
            && let Some(style) = style
        {
            self.spans.push(ExMessageSpan {
                range: start..self.text.len(),
                style,
            });
        }
    }

    /// Append a selected byte range, retaining the spans that intersect it.
    pub(super) fn append_range(&mut self, original: &Self, range: Range<usize>) {
        let start = self.text.len();
        self.text.push_str(&original.text[range.clone()]);
        let first = original
            .spans
            .partition_point(|span| span.range.end <= range.start);
        for span in &original.spans[first..] {
            if span.range.start >= range.end {
                break;
            }
            let from = span.range.start.max(range.start);
            let to = span.range.end.min(range.end);
            self.spans.push(ExMessageSpan {
                range: (start + from - range.start)..(start + to - range.start),
                style: span.style,
            });
        }
    }

    /// Look up the style at a UTF-8 byte offset without scanning earlier message lines.
    pub fn style_at(spans: &[ExMessageSpan], offset: usize) -> Option<HighlightStyle> {
        let index = spans.partition_point(|span| span.range.end <= offset);
        spans
            .get(index)
            .filter(|span| span.range.contains(&offset))
            .map(|span| span.style)
    }

    /// Filter history lines while translating highlight offsets into the resulting text.
    pub(super) fn retain_lines(&self, accept: impl Fn(&str) -> bool) -> Self {
        let mut output = Self::default();
        let mut offset = 0;
        let mut first = true;
        for raw in self.text.split_inclusive('\n') {
            let line = raw
                .strip_suffix('\n')
                .map_or(raw, |line| line.strip_suffix('\r').unwrap_or(line));
            if accept(line) {
                if !first {
                    output.text.push('\n');
                }
                output.append_range(self, offset..offset + line.len());
                first = false;
            }
            offset += raw.len();
        }
        output
    }
}

/// Return the argument to :echohl or :echoh, including an empty argument.
pub(super) fn argument(line: &str) -> Option<&str> {
    let line = line.trim();
    let (word, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    matches!(word, "echoh" | "echohl").then(|| arg.split('"').next().unwrap_or_default().trim())
}

impl Vim {
    /// Select the group for future echo output in the current workspace.
    pub(super) fn ex_echo_highlight(&mut self, text: &str, register: &Register) -> Option<Action> {
        let mut statements = super::script::statements(text);
        let group = argument(statements.next()?)?;
        if statements.next().is_some() {
            return Some(Action::ExPipeline(text.into()));
        }
        register.input_maps.set_echo_highlight(group);
        Some(Action::None)
    }

    /// Get the spans to draw beside the current Ex message text.
    pub fn ex_message_spans(&self) -> &[ExMessageSpan] {
        &self.ex_message_spans
    }

    /// Copy the currently displayed message so a modifier can preserve both text and style.
    pub fn displayed_ex_message(&self) -> ExMessageOutput {
        ExMessageOutput {
            text: self.message.clone(),
            spans: self.ex_message_spans.clone(),
        }
    }

    /// Retain the text and highlight ranges for the message area.
    pub fn publish_styled_ex_message(&mut self, output: ExMessageOutput) {
        self.message = output.text;
        self.ex_message_spans = output.spans;
        self.ex_failed = false;
    }

    /// Clear an old message so unrelated commands cannot inherit its style.
    pub fn clear_ex_message(&mut self) {
        self.message.clear();
        self.ex_message_spans.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn run(vim: &mut Vim, register: &mut Register, command: &str) -> Action {
        let action = vim.execute_ex_line(command, register);
        assert!(!vim.ex_failed(), "{command}: {}", vim.message);
        action
    }

    #[test]
    fn echo_highlighting_preserves_separate_fragments_and_resets_future_messages() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        run(
            &mut vim,
            &mut register,
            "highlight EchoOne guifg=#123456 gui=bold",
        );
        run(
            &mut vim,
            &mut register,
            "highlight EchoTwo guifg=#ABCDEF gui=underline",
        );
        run(&mut vim, &mut register, "echoh EchoOne");
        run(&mut vim, &mut register, "echo '中文'");
        run(&mut vim, &mut register, "echohl EchoTwo");
        run(&mut vim, &mut register, "echon '+'");
        run(&mut vim, &mut register, "echohl None");
        run(&mut vim, &mut register, "echon 'plain'");
        assert_eq!(vim.message, "中文+plain");
        let [first, second] = vim.ex_message_spans() else {
            panic!("expected two styled fragments");
        };
        assert_eq!(first.range, 0..6);
        assert_eq!(first.style.foreground, Some(0x123456));
        assert!(first.style.attributes.bold);
        assert_eq!(second.range, 6..7);
        assert_eq!(second.style.foreground, Some(0xABCDEF));
        assert!(second.style.attributes.underline);
        run(&mut vim, &mut register, "echo 'next'");
        assert!(vim.ex_message_spans().is_empty());
    }

    #[test]
    fn history_preserves_emitted_style_and_redirected_output_remains_plain() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        run(
            &mut vim,
            &mut register,
            "highlight EchoStored guifg=#123456",
        );
        run(&mut vim, &mut register, "redir => g:output");
        run(&mut vim, &mut register, "echohl EchoStored");
        run(&mut vim, &mut register, "echomsg \"first\\n保留\"");
        run(
            &mut vim,
            &mut register,
            "highlight EchoStored guifg=#FEDCBA",
        );
        run(&mut vim, &mut register, "echohl None");
        run(&mut vim, &mut register, "redir END");
        run(&mut vim, &mut register, "echo g:output");
        assert_eq!(vim.message, "\nfirst\n保留");
        let Action::ShowStyledListing(listing) = run(&mut vim, &mut register, "messages") else {
            panic!("expected a styled history listing");
        };
        let filtered = listing.retain_lines(|line| line == "保留");
        assert_eq!(filtered.text, "保留");
        assert_eq!(filtered.spans[0].range, 0..6);
        assert_eq!(filtered.spans[0].style.foreground, Some(0x123456));
        assert_eq!(run(&mut vim, &mut register, "messages clear"), Action::None);
        assert_eq!(run(&mut vim, &mut register, "messages"), Action::None);
    }
}
