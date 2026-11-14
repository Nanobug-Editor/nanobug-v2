//! English spelling suggestions and personal word lists for modal commands.

use super::{Mode, Pending, Vim, replay::Stroke};
use crate::{app_data, i18n};
use anyhow::{Context, Result, anyhow};
use spellbook::Dictionary;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{ErrorKind, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};
use unicode_segmentation::UnicodeSegmentation;

/// Selection keys in suggestion order, with one key for each displayed entry.
pub const SPELL_SUGGESTION_KEYS: &[u8] = b"1234567890abcdefghij";

/// Bundled SCOWL en_US dictionary from wooorm/dictionaries at 8cfea406.
static ENGLISH: LazyLock<Result<Dictionary, spellbook::ParseDictionaryError>> =
    LazyLock::new(|| {
        Dictionary::new(
            include_str!("../../assets/spelling/en_US.aff"),
            include_str!("../../assets/spelling/en_US.dic"),
        )
    });

/// Temporary good and bad entries shared by all buffers in this process.
static SESSION_WORDS: Mutex<WordList> = Mutex::new(BTreeMap::new());

/// Words keyed by their entered spelling, where true marks a valid word.
type WordList = BTreeMap<String, bool>;

/// Persistent word lists selected by the numeric prefix to a lowercase spelling command.
type PersonalWords = BTreeMap<usize, WordList>;

/// Candidate text and the original document revision and range it may replace.
pub(super) struct SpellSuggestions {
    /// Ordered, unique candidate replacements.
    words: Vec<String>,
    /// UTF-8 byte range of the current target.
    target: Range<usize>,
    /// Revision used when resolving the target and generating the suggestions.
    revision: u64,
}

impl Vim {
    /// Locate the `count`th misspelled word in the requested direction, wrapping at the file edge.
    pub(super) fn spelling_motion(&self, backward: bool, count: usize) -> Option<usize> {
        let Ok(dictionary) = ENGLISH.as_ref() else {
            return None;
        };
        let personal = personal_word_path()
            .ok()
            .and_then(|path| read_personal_words(&path).ok())
            .unwrap_or_default();
        let mut words: BTreeMap<_, _> = personal
            .into_values()
            .flatten()
            .map(|(word, good)| (word.to_lowercase(), good))
            .collect();
        words.extend(
            SESSION_WORDS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .map(|(word, &good)| (word.to_lowercase(), good)),
        );
        let offset = self.doc.cursor;
        // Start word segmentation at a line boundary so the current word stays intact.
        let row = self.doc.location(offset).0;
        let line = self.doc.line_range(row);
        let text = &self.doc.text;
        let errors = |(offset, word): (usize, &str)| {
            (word.chars().any(char::is_alphabetic)
                && !words
                    .get(&word.to_lowercase())
                    .copied()
                    .unwrap_or_else(|| dictionary.check(word)))
            .then_some(offset)
        };
        let current = || {
            text[line.clone()]
                .unicode_word_indices()
                .map(|(at, word)| (line.start + at, word))
        };
        let before = text[..line.start].unicode_word_indices();
        let after = text[line.end..]
            .unicode_word_indices()
            .map(|(at, word)| (line.end + at, word));
        let mut remaining = count.max(1);
        let mut cycle = Vec::new();
        let mut next = |item| {
            cycle.push(item);
            remaining -= 1;
            remaining == 0
        };
        let found = if backward {
            current()
                .rev()
                .skip_while(|&(at, _)| at >= offset)
                .chain(before.rev())
                .chain(after.rev())
                .chain(current().rev().take_while(|&(at, _)| at >= offset))
                .filter_map(errors)
                .any(&mut next)
        } else {
            current()
                .skip_while(|&(at, _)| at <= offset)
                .chain(after)
                .chain(before)
                .chain(current().take_while(|&(at, _)| at <= offset))
                .filter_map(errors)
                .any(&mut next)
        };
        if found {
            cycle.last().copied()
        } else if !cycle.is_empty() {
            Some(cycle[(remaining - 1) % cycle.len()])
        } else {
            None
        }
    }

    /// Return replacement words for the active spelling picker in selection-key order.
    pub fn spelling_suggestions(&self) -> Option<&[String]> {
        match &self.pending {
            Some(Pending::SpellSuggestions(choices)) => Some(&choices.words),
            _ => None,
        }
    }

    /// Apply the spelling command after z, using `count` as a list or candidate index.
    pub(super) fn spelling_command(&mut self, key: &str, count: Option<usize>) {
        if key == "=" && self.read_only {
            self.reject_edit();
            return;
        }
        let Some(target) = self.spelling_target(key == "=") else {
            self.message = i18n::text("vim_spell.no_word").into();
            return;
        };
        if key != "=" {
            self.spelling_mark(key, count, target, false);
            return;
        }

        if self.mode.is_visual() {
            self.mode = Mode::Normal;
            self.set_cursor(target.start);
        }
        let words = match suggestions(&self.doc.text[target.clone()]) {
            Ok(words) => words,
            Err(error) => {
                self.message = i18n::format("vim_spell.dictionary_error", &[("error", &error)]);
                return;
            }
        };
        if words.is_empty() {
            self.message = i18n::text("vim_spell.no_suggestions").into();
            return;
        }
        let mut choices = SpellSuggestions {
            words,
            target,
            revision: self.doc.revision,
        };
        if let Some(index) = count {
            self.replace_spelling(choices, index.saturating_sub(1));
        } else {
            choices.words.truncate(SPELL_SUGGESTION_KEYS.len());
            self.pending = Some(Pending::SpellSuggestions(choices));
        }
    }

    /// Remove a personal or temporary marking using the `key` after zu and the list `count`.
    pub(super) fn spelling_undo_key(&mut self, key: &str, count: Option<usize>) {
        if !matches!(key, "g" | "G" | "w" | "W") {
            return;
        }
        let Some(target) = self.spelling_target(false) else {
            self.message = i18n::text("vim_spell.no_word").into();
            return;
        };
        self.spelling_mark(key, count, target, true);
    }

    /// Resolve the picker `key` against the retained `choices`. Enter cancels.
    pub(super) fn spelling_choice(&mut self, key: &str, choices: SpellSuggestions) {
        if let [choice] = key.as_bytes()
            && let Some(index) = SPELL_SUGGESTION_KEYS.iter().position(|key| key == choice)
            && index < choices.words.len()
        {
            self.replace_spelling(choices, index);
        } else if key != "Enter" {
            self.pending = Some(Pending::SpellSuggestions(choices));
        }
    }

    /// Resolve a same-line selection or the word under the caret, optionally searching forward.
    fn spelling_target(&self, use_following_word: bool) -> Option<Range<usize>> {
        let selection = match self.mode {
            Mode::Visual | Mode::VisualLine => self.selection(),
            Mode::VisualBlock => self.selection_on_line(self.doc.location(self.doc.cursor).0),
            _ => None,
        };
        if let Some(range) = selection {
            let raw = &self.doc.text[range.clone()];
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.contains('\n') {
                return None;
            }
            let start = range.start + raw.len() - raw.trim_start().len();
            return Some(start..start + trimmed.len());
        }

        let line = self.doc.line_range(self.doc.location(self.doc.cursor).0);
        let column = self.doc.cursor.saturating_sub(line.start);
        self.doc.text[line.clone()]
            .unicode_word_indices()
            .find(|(start, word)| {
                column < start + word.len()
                    && (use_following_word || column >= *start)
                    && word.chars().any(char::is_alphabetic)
            })
            .map(|(start, word)| line.start + start..line.start + start + word.len())
    }

    /// Update the spelling entry for `target`, using `key` to select polarity and scope.
    /// The optional `count` selects a persistent list and `remove` clears the marking.
    fn spelling_mark(
        &mut self,
        key: &str,
        count: Option<usize>,
        target: Range<usize>,
        remove: bool,
    ) {
        let word = self.doc.text[target].to_owned();
        let good = matches!(key, "g" | "G");
        let result = if matches!(key, "G" | "W") {
            let mut list = SESSION_WORDS
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            update_word(&mut list, &word, good, remove);
            Ok(())
        } else {
            personal_word_path().and_then(|path| {
                update_personal_words(&path, count.unwrap_or(1), &word, good, remove)
            })
        };
        if let Err(error) = result {
            self.message = i18n::format("vim_spell.dictionary_error", &[("error", &error)]);
            return;
        }
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
            self.clamp_normal();
        }
        let key = if remove {
            "vim_spell.removed"
        } else if good {
            "vim_spell.good"
        } else {
            "vim_spell.bad"
        };
        self.message = i18n::format(key, &[("word", &word)]);
    }

    /// Apply the zero-based `index` in `choices` when its revision is still current.
    fn replace_spelling(&mut self, choices: SpellSuggestions, index: usize) {
        if choices.revision != self.doc.revision {
            return;
        }
        let count = choices.words.len();
        let Some(word) = choices.words.into_iter().nth(index) else {
            self.message = i18n::format("vim_spell.suggestion_count", &[("count", &count)]);
            return;
        };
        let start = choices.target.start;
        self.replace(choices.target, &word, true);
        self.set_cursor(start);
        // Dot repeat uses the chosen text, independent of candidate order for the next word.
        self.replay.sequence = vec![
            Stroke::Key("c".into()),
            Stroke::Key("i".into()),
            Stroke::Key("w".into()),
            Stroke::Text(word),
            Stroke::Key("Escape".into()),
        ];
    }
}

/// Return the current installation's portable word-list file.
fn personal_word_path() -> Result<PathBuf> {
    Ok(app_data::directory_for(&env::current_exe()?)?.join("spell-words.json"))
}

/// Read the personal lists at `path`; a new installation starts with none.
fn read_personal_words(path: &Path) -> Result<PersonalWords> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Parse spelling word lists"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error).context("Read spelling word lists"),
    }
}

/// Record the `good` marking for `word` in `list`, or delete the entry when `remove` is set.
/// Report whether the stored marking changed.
fn update_word(list: &mut WordList, word: &str, good: bool, remove: bool) -> bool {
    if remove {
        list.remove(word).is_some()
    } else {
        list.insert(word.to_owned(), good) != Some(good)
    }
}

/// Change `word` in the numbered personal `index` at `path` without losing other writers.
/// `good` selects the marking; `remove` deletes that marking instead of adding it.
fn update_personal_words(
    path: &Path,
    index: usize,
    word: &str,
    good: bool,
    remove: bool,
) -> Result<()> {
    let _lock = app_data::lock_data_file(path)?;
    let mut lists = read_personal_words(path)?;
    let list = lists.entry(index).or_default();
    if !update_word(list, word, good, remove) {
        return Ok(());
    }
    if list.is_empty() {
        lists.remove(&index);
    }
    let dir = path
        .parent()
        .context("Spelling word lists have no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    staged.write_all(&serde_json::to_vec_pretty(&lists)?)?;
    staged.as_file().sync_all()?;
    staged.persist(path).context("Save spelling word lists")?;
    Ok(())
}

/// Generate candidates for `word` using the bundled dictionary and explicit word markings.
fn suggestions(word: &str) -> Result<Vec<String>> {
    let dictionary = ENGLISH.as_ref().map_err(|error| anyhow!("{error}"))?;
    let personal = read_personal_words(&personal_word_path()?)?;
    let mut overrides: BTreeMap<_, _> = personal
        .into_values()
        .flatten()
        .map(|(word, good)| (word.to_lowercase(), (word, good)))
        .collect();
    let session = SESSION_WORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    overrides.extend(
        session
            .iter()
            .map(|(word, &good)| (word.to_lowercase(), (word.clone(), good))),
    );
    drop(session);
    let mut words = Vec::new();
    dictionary.suggest(word, &mut words);
    let mut seen = BTreeSet::new();
    words.retain(|candidate| {
        candidate != word
            && overrides
                .get(&candidate.to_lowercase())
                .is_none_or(|(_, good)| *good)
            && seen.insert(candidate.clone())
    });
    let lowered: Vec<_> = word.to_lowercase().chars().collect();
    for (key, (candidate, good)) in &overrides {
        if *good && similar(&lowered, key) && seen.insert(candidate.clone()) {
            words.push(candidate.clone());
        }
    }
    Ok(words)
}

/// Whether the lowered `word` and `candidate` differ by one edit or adjacent transposition.
fn similar(word: &[char], candidate: &str) -> bool {
    let candidate: Vec<_> = candidate.chars().collect();
    let prefix = word
        .iter()
        .zip(&candidate)
        .take_while(|(a, b)| a == b)
        .count();
    let left = &word[prefix..];
    let right = &candidate[prefix..];
    match left.len().cmp(&right.len()) {
        std::cmp::Ordering::Greater => left[1..] == *right,
        std::cmp::Ordering::Less => *left == right[1..],
        std::cmp::Ordering::Equal if !left.is_empty() => {
            left[1..] == right[1..]
                || (left.len() > 1
                    && left[0] == right[1]
                    && left[1] == right[0]
                    && left[2..] == right[2..])
        }
        std::cmp::Ordering::Equal => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Offer an actual dictionary candidate, apply it through normal keys and undo the edit.
    #[test]
    fn spelling_suggestion_replaces_the_word_and_is_undoable() {
        let mut vim = Vim::new(Document::new("teh teh\n"));
        let mut register = Register::default();
        vim.text("z=", &mut register);
        let options = vim.spelling_suggestions().unwrap();
        let index = options.iter().position(|word| word == "the").unwrap();
        let key = char::from(SPELL_SUGGESTION_KEYS[index]).to_string();
        vim.key(&key, &mut register);
        assert_eq!(vim.doc.text, "the teh\n");
        assert_eq!(vim.doc.cursor, 0);
        vim.text("w.", &mut register);
        assert_eq!(vim.doc.text, "the the\n");
        vim.text("uu", &mut register);
        assert_eq!(vim.doc.text, "teh teh\n");
        vim.text(&format!("{}z=", index + 1), &mut register);
        assert!(vim.doc.text.contains("the"));
        assert!(vim.spelling_suggestions().is_none());
    }

    /// Persist numbered lists and remove only markings in the requested list.
    #[test]
    fn personal_spell_lists_persist_independently() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("spell.json");
        update_personal_words(&path, 1, "Nanobug", true, false)?;
        update_personal_words(&path, 2, "Nanobug", false, false)?;
        update_personal_words(&path, 1, "Nanobug", false, true)?;
        let lists = read_personal_words(&path)?;
        assert!(!lists.contains_key(&1));
        assert!(!lists[&2]["Nanobug"]);
        Ok(())
    }

    /// Uppercase spell commands share transient words across buffers and undo either marking.
    #[test]
    fn uppercase_spelling_marks_remain_in_the_session() {
        let mut vim = Vim::new(Document::new("nanobugtesttemporaryword\n"));
        let mut register = Register::default();
        let word = vim.doc.line(0).to_owned();
        vim.text("zG", &mut register);
        assert_eq!(SESSION_WORDS.lock().unwrap().get(&word), Some(&true));
        let mut vim = Vim::new(Document::new(&word));
        vim.text("zuW", &mut register);
        assert_eq!(SESSION_WORDS.lock().unwrap().get(&word), None);
        vim.text("zW", &mut register);
        assert_eq!(SESSION_WORDS.lock().unwrap().get(&word), Some(&false));
        vim.text("zuG", &mut register);
        assert_eq!(SESSION_WORDS.lock().unwrap().get(&word), None);
    }

    /// Cancelling a suggestion leaves the buffer untouched, including in a preview.
    #[test]
    fn spelling_picker_can_cancel_and_rejects_read_only_edits() {
        let text = "teh";
        let mut vim = Vim::new(Document::new(text));
        let mut register = Register::default();
        vim.text("z=", &mut register);
        assert!(vim.spelling_suggestions().is_some());
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.text, text);
        assert!(vim.spelling_suggestions().is_none());

        let mut preview = Vim::read_only(Document::new(text));
        preview.text("1z=", &mut register);
        assert_eq!(preview.doc.text, text);
        assert!(preview.spelling_suggestions().is_none());
    }
}
