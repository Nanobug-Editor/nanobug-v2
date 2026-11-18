//! Text registers and recorded key sequences shared by an editor's buffers.

use super::{FileMarks, command_prompt::Histories, replay::Stroke};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

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

/// A register slot that Ex output can update after the originating window loses focus.
pub(super) type RedirectedRegister = Arc<Mutex<Contents>>;

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
    /// Shared slots allocated only for registers selected as redirection destinations.
    redirected: BTreeMap<char, RedirectedRegister>,
    /// Key tokens for registers filled by macro recording.
    macros: BTreeMap<char, Vec<Stroke>>,
    /// Target and keys of the current recording.
    pub(super) recording: Option<(char, Vec<Stroke>)>,
    /// Most recently played and recorded register names.
    pub(super) last_played: Option<char>,
    pub(super) last_recorded: Option<char>,
    /// Register name most recently executed through the Ex :@ command.
    pub(super) last_ex_register: Option<char>,
    /// Counter for successful yanks, including identical text copied twice.
    yank_revision: u64,
    /// Ex and search input history shared by this pane's buffers.
    pub(super) command_history: Histories,
    /// Last typed expression value prepared for a Normal or Visual put.
    pub(super) expression_contents: Option<Contents>,
    /// Last submitted expression-register source, reused by an empty prompt.
    pub(super) expression_source: String,
    /// Expression source used by the most recent @= playback.
    pub(super) played_expression: String,
    /// Uppercase file marks shared by all editors of one Session.
    pub(super) file_marks: FileMarks,
    /// Latest portable caret history used for marks 0–9.
    pub(super) numbered_marks: std::sync::Arc<[crate::app_data::numbered_marks::Mark]>,
    /// Configured operator function and definitions shared across this editor's buffers.
    pub(super) scripts: super::script::State,
    /// Semantic input from the last completed Insert or Replace segment.
    pub(super) last_insert: Vec<Stroke>,
    /// Shared user mappings and abbreviations, independently of register contents.
    pub(super) input_maps: super::InputMappings,
    /// Display options for the window containing this register bank.
    pub(super) window_options: Option<super::WindowOptions>,
    /// Match patterns shared by the buffers in this window.
    pub(super) ex_matches: [Option<super::ExMatch>; 3],
}

impl Register {
    /// Return the window's three match slots with the highest priority first.
    pub fn ex_matches(&self) -> &[Option<super::ExMatch>; 3] {
        &self.ex_matches
    }

    /// Attach the Session's shared `marks` without changing this window's other registers.
    pub fn set_file_marks(&mut self, marks: FileMarks) {
        self.file_marks = marks;
    }

    /// Connect stored marks belonging to `doc` to its live position tracking.
    pub fn bind_file_marks(&self, doc: &crate::document::Document) {
        self.file_marks.bind(doc);
    }

    /// Format populated registers, restricted to the optional names in `filter`.
    pub(super) fn ex_listing(&self, filter: &str) -> String {
        use std::{collections::BTreeSet, fmt::Write as _};
        let names = if filter.is_empty() {
            self.named
                .keys()
                .chain(self.redirected.keys())
                .chain(self.macros.keys())
                .copied()
                .chain(['"', '.', ':', '='])
                .collect::<BTreeSet<_>>()
        } else {
            filter.chars().filter(|ch| !ch.is_whitespace()).collect()
        };
        let mut listing = String::from("Type Name Content\n");
        for name in names {
            let value = self.read(Some(name));
            let text = if let Some(strokes) = self.macros.get(&name) {
                strokes
                    .iter()
                    .map(Stroke::register_text)
                    .collect::<String>()
            } else {
                value.text
            };
            if text.is_empty() {
                continue;
            }
            let kind = match value.kind {
                Kind::Character => 'c',
                Kind::Line => 'l',
                Kind::Block(_) => 'b',
            };
            let _ = writeln!(
                listing,
                "{kind:>4}  \"{name}   {}",
                text.replace('\n', "^J")
            );
        }
        listing
    }

    /// Use the workspace's `mappings` for definitions made without `<buffer>`.
    pub fn set_input_mappings(&mut self, mappings: super::InputMappings) {
        self.window_options
            .get_or_insert_with(|| mappings.options().window);
        self.input_maps = mappings;
    }

    /// Read the workspace's input definitions and Ex presentation state from this pane.
    pub fn input_mappings(&self) -> &super::InputMappings {
        &self.input_maps
    }

    /// Refresh shared numbered `marks` after loading or saving the editor's portable history.
    pub fn set_numbered_marks(
        &mut self,
        marks: std::sync::Arc<[crate::app_data::numbered_marks::Mark]>,
    ) {
        self.numbered_marks = marks;
    }

    /// Whether the current keystrokes are being recorded into a macro.
    pub fn is_recording_macro(&self) -> bool {
        self.recording.is_some()
    }

    /// Return the revision of the most recent yank.
    pub fn yank_revision(&self) -> u64 {
        self.yank_revision
    }

    /// Read `name`, defaulting to the unnamed register.
    pub(super) fn read(&self, name: Option<char>) -> Contents {
        if let Some(slot) = self
            .redirected
            .get(&name.unwrap_or('"').to_ascii_lowercase())
        {
            return slot.lock().unwrap().clone();
        }
        if name == Some('=') {
            return self.expression_contents.clone().unwrap_or_default();
        }
        let special = match name {
            Some(':') => self
                .command_history
                .latest(':')
                .unwrap_or_default()
                .to_owned(),
            Some('.') => self
                .last_insert
                .iter()
                .map(|stroke| match stroke {
                    Stroke::Key(key) if matches!(key.as_str(), "Backspace" | "Ctrl+h") => "\u{8}",
                    _ => stroke.register_text(),
                })
                .collect(),
            _ => String::new(),
        };
        if matches!(name, Some(':' | '.')) {
            return Contents {
                text: special,
                kind: Kind::Character,
            };
        }
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
        if matches!(name, Some('_' | ':' | '.' | '%' | '#' | '/' | '=')) {
            return;
        }
        self.macros.remove(&'"');
        if yank {
            self.yank_revision = self.yank_revision.wrapping_add(1);
        }
        if let Some(name) = name.filter(|&name| name != '"') {
            let target = name.to_ascii_lowercase();
            self.macros.remove(&target);
            if name.is_ascii_uppercase()
                && (self.named.contains_key(&target) || self.redirected.contains_key(&target))
            {
                let mut old = self.read(Some(target));
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
                value = old;
            }
            self.store_named(target, value.clone());
        } else if yank {
            self.store_named('0', value.clone());
        } else if value.kind == Kind::Line || value.text.contains('\n') {
            for n in ('1'..='8').rev() {
                let previous = if let Some(slot) = self.redirected.get(&n) {
                    Some(std::mem::take(&mut *slot.lock().unwrap()))
                } else {
                    self.named.remove(&n)
                };
                if let Some(previous) = previous {
                    self.store_named(char::from(n as u8 + 1), previous);
                }
            }
            self.store_named('1', value.clone());
        } else {
            self.store_named('-', value.clone());
        }
        self.store_unnamed(value);
    }

    /// Store in a named slot, keeping an active capture attached.
    fn store_named(&mut self, name: char, value: Contents) {
        if let Some(slot) = self.redirected.get(&name) {
            *slot.lock().unwrap() = value;
        } else {
            self.named.insert(name, value);
        }
    }

    /// Track both the latest ordinary transfer and the unnamed capture slot.
    fn store_unnamed(&mut self, value: Contents) {
        if let Some(slot) = self.redirected.get(&'"') {
            *slot.lock().unwrap() = value.clone();
        }
        self.text = value.text;
        self.linewise = value.kind == Kind::Line;
        self.block = match value.kind {
            Kind::Block(width) => Some(width),
            _ => None,
        };
    }

    /// Start a live capture without changing yank or delete history.
    pub(super) fn redirect_to_register(&mut self, name: char, append: bool) -> RedirectedRegister {
        let name = name.to_ascii_lowercase();
        let contents = if append {
            self.read(Some(name))
        } else {
            Contents::default()
        };
        self.macros.remove(&name);
        self.named.remove(&name);
        let slot = Arc::new(Mutex::new(contents));
        self.redirected.insert(name, slot.clone());
        slot
    }

    /// Iterate words from the unnamed and explicitly named text registers for completion.
    pub(super) fn completion_texts(&self) -> impl Iterator<Item = Cow<'_, str>> {
        let unnamed = self
            .redirected
            .get(&'"')
            .map_or(Cow::Borrowed(self.text.as_str()), |slot| {
                Cow::Owned(slot.lock().unwrap().text.clone())
            });
        std::iter::once(unnamed)
            .chain(
                self.named
                    .values()
                    .map(|value| Cow::Borrowed(value.text.as_str())),
            )
            .chain(
                self.redirected
                    .iter()
                    .filter(|(name, _)| **name != '"')
                    .map(|(_, slot)| Cow::Owned(slot.lock().unwrap().text.clone())),
            )
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
        let text = keys.iter().map(Stroke::register_text).collect();
        let value = Contents {
            text,
            kind: Kind::Character,
        };
        if target == '"' {
            self.store_unnamed(value);
        } else {
            self.store_named(target, value);
        }
        self.macros.insert(target, keys);
    }

    /// Read executable keys from a recorded or yanked `name`.
    pub(super) fn macro_keys(&self, name: char) -> Vec<Stroke> {
        let name = name.to_ascii_lowercase();
        if let Some(keys) = self.macros.get(&name) {
            return keys.clone();
        }
        text_keys(&self.read(Some(name)).text)
    }
}

/// Decode pasted register text into semantic keystrokes for macro playback.
pub(super) fn text_keys(text: &str) -> Vec<Stroke> {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true)
        .map(|key| Stroke::Key(text_key(key)))
        .collect()
}

/// Decode a single `key` grapheme from literal Vim key text.
pub(super) fn text_key(key: &str) -> String {
    match key {
        "\n" | "\r" | "\r\n" => "Enter",
        "\t" => "Tab",
        "\u{1b}" => "Escape",
        "\u{8}" => "Backspace",
        "\u{f700}" => "Up",
        "\u{f701}" => "Down",
        "\u{f702}" => "Left",
        "\u{f703}" => "Right",
        _ => {
            if let [byte @ 1..=26] = key.as_bytes() {
                return format!("Ctrl+{}", char::from(*byte + b'a' - 1));
            }
            key
        }
    }
    .into()
}
