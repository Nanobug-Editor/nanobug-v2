//! Text registers and recorded key sequences shared by an editor's buffers.

use super::replay::Stroke;
use std::collections::BTreeMap;

/// Paste geometry of a register's contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Kind {
    /// Insert text at the caret.
    #[default]
    Character,
    /// Insert whole lines above or below the current line.
    Line,
    /// Insert each segment at the same display column, with this minimum width.
    Block(usize),
}

/// Copied text and its shape.
#[derive(Clone, Default)]
pub(super) struct Contents {
    /// UTF-8 text, with LF separating lines or rows.
    pub text: String,
    /// Shape retained by yank and put operations.
    pub kind: Kind,
}

/// Unnamed register and named register bank, shared across open buffers.
#[derive(Default)]
pub struct Register {
    /// Most recent yank or delete.
    pub text: String,
    /// Whether the unnamed register contains complete lines.
    pub linewise: bool,
    /// Rectangular width of the unnamed register, when applicable.
    block: Option<usize>,
    /// Contents of explicitly named registers.
    named: BTreeMap<char, Contents>,
    /// Key tokens for registers filled by macro recording.
    macros: BTreeMap<char, Vec<Stroke>>,
    /// Target and keys of the current recording.
    pub(super) recording: Option<(char, Vec<Stroke>)>,
    /// Most recently played and recorded register names.
    pub(super) last_played: Option<char>,
    pub(super) last_recorded: Option<char>,
    /// Counter for successful yanks, including identical text copied twice.
    yank_revision: u64,
}

impl Register {
    /// Return the revision of the most recent yank.
    pub fn yank_revision(&self) -> u64 {
        self.yank_revision
    }

    /// Read `name`, defaulting to the unnamed register.
    pub(super) fn read(&self, name: Option<char>) -> Contents {
        if let Some(name) = name.filter(|&name| name != '"') {
            return self
                .named
                .get(&name.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default();
        }
        Contents {
            text: self.text.clone(),
            kind: if self.linewise {
                Kind::Line
            } else {
                self.block.map_or(Kind::Character, Kind::Block)
            },
        }
    }

    /// Store a yank or deletion in `name` and update the default registers.
    pub(super) fn write(&mut self, name: Option<char>, mut value: Contents, yank: bool) {
        if name == Some('_') {
            return;
        }
        if yank {
            self.yank_revision = self.yank_revision.wrapping_add(1);
        }
        if let Some(name) = name.filter(|&name| name != '"') {
            let target = name.to_ascii_lowercase();
            self.macros.remove(&target);
            if name.is_ascii_uppercase()
                && let Some(old) = self.named.get_mut(&target)
            {
                if value.kind == Kind::Line && !old.text.ends_with('\n') {
                    old.text.push('\n');
                }
                old.text.push_str(&value.text);
                if old.kind == Kind::Line || value.kind == Kind::Line {
                    old.kind = Kind::Line;
                    if !old.text.ends_with('\n') {
                        old.text.push('\n');
                    }
                } else {
                    old.kind = value.kind;
                }
                value = old.clone();
            } else {
                self.named.insert(target, value.clone());
            }
        } else if yank {
            self.named.insert('0', value.clone());
        } else if value.kind == Kind::Line || value.text.contains('\n') {
            for n in ('1'..='8').rev() {
                if let Some(previous) = self.named.remove(&n) {
                    self.named.insert(char::from(n as u8 + 1), previous);
                }
            }
            self.named.insert('1', value.clone());
        } else {
            self.named.insert('-', value.clone());
        }
        self.text = value.text;
        self.linewise = value.kind == Kind::Line;
        self.block = match value.kind {
            Kind::Block(width) => Some(width),
            _ => None,
        };
    }

    /// Commit the recording to its register. Uppercase names append.
    pub(super) fn stop_recording(&mut self) {
        let Some((name, mut keys)) = self.recording.take() else {
            return;
        };
        let target = name.to_ascii_lowercase();
        if name.is_ascii_uppercase() {
            let mut previous = self.macro_keys(target);
            previous.append(&mut keys);
            keys = previous;
        }
        self.last_recorded = Some(target);
        self.named.insert(
            target,
            Contents {
                text: keys.iter().map(Stroke::register_text).collect(),
                kind: Kind::Character,
            },
        );
        self.macros.insert(target, keys);
    }

    /// Read executable keys from a recorded or yanked `name`.
    pub(super) fn macro_keys(&self, name: char) -> Vec<Stroke> {
        let name = name.to_ascii_lowercase();
        if let Some(keys) = self.macros.get(&name) {
            return keys.clone();
        }
        let value = self.read(Some(name));
        use unicode_segmentation::UnicodeSegmentation;
        value
            .text
            .graphemes(true)
            .map(|key| {
                Stroke::Key(
                    match key {
                        "\n" => "Enter",
                        "\t" => "Tab",
                        "\u{1b}" => "Escape",
                        _ => key,
                    }
                    .into(),
                )
            })
            .collect()
    }
}
