//! Identify and execute explicit Vim examples in read-only Help buffers.

use super::{Register, Vim};
use crate::{document::Document, i18n};

impl Vim {
    /// Open `doc` as a read-only Vim Help buffer with g== enabled.
    pub fn help(doc: Document) -> Self {
        let mut result = Self::read_only(doc);
        result.help_buffer = true;
        result
    }

    /// Whether this buffer is presented as Help and recognizes its local mappings.
    pub fn is_help(&self) -> bool {
        self.help_buffer
    }

    /// Run the Vim example under the caret with the pane's `register`.
    pub(super) fn run_help_code(&mut self, register: &mut Register) {
        if register.scripts.running_help {
            self.script_error("A Help code block is already running");
            return;
        }
        let row = self.doc.location(self.doc.cursor).0;
        let Some(source) = self.help_block(row) else {
            self.message = i18n::text("vim_g.help_no_block").into();
            return;
        };
        register.scripts.running_help = true;
        let result = self.run_script(&source, register);
        register.scripts.running_help = false;
        if let Err(error) = result {
            self.script_error(&error);
        }
    }

    /// Return the indented Vim example covering `row`, bounded by a closing marker or prose.
    fn help_block(&self, row: usize) -> Option<String> {
        let mut opener = None;
        for index in 0..=row {
            let line = self.doc.line(index);
            if is_vim_opener(line) {
                opener = Some(index);
            } else if line == "<" || !line.is_empty() && !line.starts_with([' ', '\t']) {
                opener = None;
            }
        }
        let start = opener?;
        let end = (start + 1..self.line_count())
            .find(|&index| {
                let line = self.doc.line(index);
                !line.is_empty() && !line.starts_with([' ', '\t'])
            })
            .unwrap_or(self.line_count());
        if start + 1 == end || row >= end {
            return None;
        }
        Some(
            (start + 1..end)
                .map(|index| self.doc.line(index))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// Recognize a Vim example opener at the end of a Help `line`.
fn is_vim_opener(line: &str) -> bool {
    line.trim_end()
        .strip_suffix(">vim")
        .is_some_and(|before| before.is_empty() || before.ends_with(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::Action;

    /// Run a Vim example from its own buffer and expose the nested g= hint.
    #[test]
    fn help_code_is_local_and_evaluates_vim() {
        let doc = Document::new(
            "Examples >vim\n  let values = [2, 4, 8]\n  echo join(values, ', ')\n<\n",
        );
        let mut vim = Vim::help(doc);
        let mut register = Register::default();
        vim.move_to_row(2);
        assert_eq!(vim.text("g=", &mut register), Action::None);
        assert_eq!(vim.sequence_hint().unwrap().prefix, "g=");
        vim.text("=", &mut register);
        assert_eq!(vim.message, "2, 4, 8");
        vim.move_to_row(3);
        vim.text("g==", &mut register);
        assert_eq!(vim.message, i18n::text("vim_g.help_no_block"));
    }

    /// Keep unsupported examples readable as text when a local help file contains them.
    #[test]
    fn unsupported_help_example_stays_as_text() {
        let text = ">vim\n  echo 42\n<\n>lua\n  print(43)\n<\n";
        let mut vim = Vim::help(Document::new(text));
        vim.move_to_row(4);
        vim.text("g==", &mut Register::default());
        assert_eq!(vim.message, i18n::text("vim_g.help_no_block"));
        assert_eq!(vim.doc.text, text);
    }
}
