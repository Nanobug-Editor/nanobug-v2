//! English spelling suggestions and personal word lists for modal commands.

use super::{Action, Mode, Pending, Vim, replay::Stroke};
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

/// Old portable word lists stored `Good` and `Bad` as booleans.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
enum SpellingMark {
    Valid(bool),
    Rare(RareMark),
}

/// Serialized spelling classification for a rare word.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum RareMark {
    Rare,
}

/// Explicit overrides keyed by entered spelling.
type WordList = BTreeMap<String, SpellingMark>;

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
    /// Execute spelling operations with a literal word and an optional personal list index.
    pub(super) fn ex_spell_command(&mut self, text: &str) -> Option<Action> {
        let end = text.bytes().take_while(u8::is_ascii_digit).count();
        let (index, body) = text.split_at(end);
        let body = body.trim_start();
        let split = body.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (name, args) = body.split_at(split);
        let is = |full: &str, minimum| name.len() >= minimum && full.starts_with(name);
        let (temporary, args) = args
            .strip_prefix('!')
            .map_or((false, args), |args| (true, args));
        let args = args.trim();
        if is("spellrepall", 6) {
            if temporary || !index.is_empty() || !args.is_empty() {
                self.pattern_error("Invalid spelling argument");
            } else {
                self.replace_all_spelling();
            }
            return Some(Action::None);
        }
        if is("spellinfo", 6) || is("spelldump", 6) {
            if !index.is_empty() || !args.is_empty() || temporary && is("spellinfo", 6) {
                self.pattern_error("Invalid spelling argument");
                return Some(Action::None);
            }
            let result = if is("spellinfo", 6) {
                ex_dictionary_info()
            } else if temporary {
                self.ex_failed = true;
                self.message = i18n::text("vim_spell.display_frequency_unavailable").into();
                return Some(Action::None);
            } else {
                ex_dictionary_dump()
            };
            return Some(match result {
                Ok(info) if is("spellinfo", 6) => {
                    self.message = info;
                    Action::None
                }
                Ok(words) => Action::EditableListing(words),
                Err(error) => {
                    self.ex_failed = true;
                    self.message = i18n::format("vim_spell.dictionary_error", &[("error", &error)]);
                    Action::None
                }
            });
        }
        let (mark, remove) = if is("spellgood", 3) {
            (SpellingMark::Valid(true), false)
        } else if is("spellwrong", 6) {
            (SpellingMark::Valid(false), false)
        } else if is("spellrare", 7) {
            (SpellingMark::Rare(RareMark::Rare), false)
        } else if is("spellundo", 6) {
            (SpellingMark::Valid(false), true)
        } else {
            return None;
        };
        let index = if index.is_empty() {
            Some(1)
        } else {
            index.parse::<usize>().ok().filter(|&n| n > 0)
        };
        if args.is_empty()
            || args.contains(['\n', '\r', '\0'])
            || index.is_none()
            || temporary && index != Some(1)
        {
            self.pattern_error("Expected a word and a valid spelling list number");
            return Some(Action::None);
        }
        self.mark_spell_word(args, mark, temporary, index.unwrap(), remove);
        Some(Action::None)
    }

    /// Offer dictionary replacements for the nearest misspelled word before the Insert caret.
    /// When all words are spelled correctly, fall back to the nearest preceding word.
    pub(super) fn insert_spelling_candidates(&mut self) -> super::Action {
        let cursor = self.doc.cursor;
        let line = self.doc.line_range(self.doc.location(cursor).0);
        let text = &self.doc.text[line.clone()];
        let mut words = text
            .unicode_word_indices()
            .rev()
            .filter(|(at, word)| line.start + at < cursor && word.chars().any(char::is_alphabetic));
        let Some(current) = words.next() else {
            return super::Action::None;
        };
        let invalid = |word| {
            ENGLISH
                .as_ref()
                .is_ok_and(|dictionary| !dictionary.check(word))
        };
        let (at, word) = if invalid(current.1) {
            current
        } else {
            words.find(|(_, word)| invalid(word)).unwrap_or(current)
        };
        let values = suggestions(word).unwrap_or_default();
        if values.is_empty() {
            return super::Action::None;
        }
        let start = line.start + at;
        self.doc.cursor = start + word.len();
        super::Action::InsertCandidates {
            start,
            values,
            reverse: false,
        }
    }

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
                && words.get(&word.to_lowercase()).map_or_else(
                    || !dictionary.check(word),
                    |&mark| mark != SpellingMark::Valid(true),
                ))
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
        if !self.mark_spell_word(
            &word,
            SpellingMark::Valid(good),
            matches!(key, "G" | "W"),
            count.unwrap_or(1),
            remove,
        ) {
            return;
        }
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
            self.clamp_normal();
        }
    }

    /// Mark the literal `word` in the numbered personal or temporary list and show the result.
    fn mark_spell_word(
        &mut self,
        word: &str,
        mark: SpellingMark,
        temporary: bool,
        index: usize,
        remove: bool,
    ) -> bool {
        let result = if temporary {
            let mut list = SESSION_WORDS
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            update_word(&mut list, word, mark, remove);
            Ok(())
        } else {
            personal_word_path()
                .and_then(|path| update_personal_words(&path, index, word, mark, remove))
        };
        if let Err(error) = result {
            self.ex_failed = true;
            self.message = i18n::format("vim_spell.dictionary_error", &[("error", &error)]);
            return false;
        }
        let key = if remove {
            "vim_spell.removed"
        } else {
            match mark {
                SpellingMark::Valid(true) => "vim_spell.good",
                SpellingMark::Valid(false) => "vim_spell.bad",
                SpellingMark::Rare(_) => "vim_spell.rare",
            }
        };
        self.message = i18n::format(key, &[("word", &word)]);
        true
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
        self.options.set_spelling_replacement(
            self.doc.text[choices.target.clone()].to_owned(),
            word.clone(),
        );
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

    /// Repeat the most recent suggestion for exact word occurrences in this buffer as one edit.
    fn replace_all_spelling(&mut self) {
        if self.is_read_only() {
            self.pattern_error("Cannot replace spelling in a read-only buffer");
            return;
        }
        let Some((from, to)) = self.options.spelling_replacement() else {
            self.ex_failed = true;
            self.message = i18n::text("vim_spell.no_replacement").into();
            return;
        };
        let ranges = self
            .doc
            .text
            .match_indices(&from)
            .filter_map(|(start, _)| {
                let end = start + from.len();
                let keyword = |ch: char| ch.is_alphanumeric() || ch == '_';
                (!self.doc.text[..start]
                    .chars()
                    .next_back()
                    .is_some_and(keyword)
                    && !self.doc.text[end..].chars().next().is_some_and(keyword))
                .then_some(start..end)
            })
            .collect::<Vec<_>>();
        if ranges.is_empty() {
            self.ex_failed = true;
            self.message = i18n::text("vim_spell.no_matches").into();
            return;
        }
        let cursor = self.doc.cursor;
        let count = ranges.len();
        for (index, range) in ranges.into_iter().rev().enumerate() {
            self.replace(range, &to, index == 0);
        }
        self.set_cursor(cursor.min(self.doc.text.len()));
        self.message = i18n::format("vim_spell.replaced_all", &[("count", &count)]);
    }
}

/// List the installed language and the active personal and temporary word sources.
fn ex_dictionary_info() -> Result<String> {
    ENGLISH.as_ref().map_err(|error| anyhow!("{error}"))?;
    let path = personal_word_path()?;
    let lists = read_personal_words(&path)?;
    let temporary = SESSION_WORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .len();
    Ok(format!(
        "en_US (bundled English) · {} ({} lists) · {temporary} temporary entries",
        path.display(),
        lists.len()
    ))
}

/// Materialize valid noncompound dictionary words with personal and temporary overrides.
fn ex_dictionary_dump() -> Result<String> {
    let dictionary = ENGLISH.as_ref().map_err(|error| anyhow!("{error}"))?;
    let affixes = english_affixes()?;
    let mut entries: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    let mut add = |word: &str| {
        if !word.is_empty() && dictionary.check(word) {
            entries
                .entry(word.to_lowercase())
                .or_default()
                .insert(word.to_owned());
        }
    };
    for entry in include_str!("../../assets/spelling/en_US.dic")
        .lines()
        .skip(1)
    {
        let entry = entry.split_whitespace().next().unwrap_or("");
        let (word, flags) = entry.split_once('/').unwrap_or((entry, ""));
        add(word);
        let rules = flags
            .chars()
            .filter_map(|flag| affixes.get(&flag))
            .flatten()
            .collect::<Vec<_>>();
        for &rule in &rules {
            if let Some(derived) = rule.apply(word) {
                add(&derived);
                if rule.before && rule.crosses {
                    for suffix in rules
                        .iter()
                        .copied()
                        .filter(|suffix| !suffix.before && suffix.crosses)
                    {
                        if let Some(combined) = suffix.apply(&derived) {
                            add(&combined);
                        }
                    }
                }
            }
        }
    }
    let personal = read_personal_words(&personal_word_path()?)?;
    for (word, good) in personal.into_values().flatten().chain(
        SESSION_WORDS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
    ) {
        if good != SpellingMark::Valid(false) {
            entries.insert(word.to_lowercase(), BTreeSet::from([word]));
        } else {
            entries.remove(&word.to_lowercase());
        }
    }
    Ok(entries
        .into_values()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"))
}

/// A prefix or suffix rule from the bundled single-character flag dictionary.
struct EnglishAffix {
    /// Apply at the beginning of the word. A false value identifies a suffix.
    before: bool,
    /// Whether one prefix can be combined with one suffix.
    crosses: bool,
    /// Existing word edge to remove.
    strip: &'static str,
    /// New prefix or suffix to add.
    append: &'static str,
    /// Constraint at the original word edge.
    condition: regex::Regex,
}

impl EnglishAffix {
    /// Derive a word from this rule's edge and condition.
    fn apply(&self, word: &str) -> Option<String> {
        if !self.condition.is_match(word) {
            return None;
        }
        if self.before {
            Some(format!("{}{}", self.append, word.strip_prefix(self.strip)?))
        } else {
            Some(format!("{}{}", word.strip_suffix(self.strip)?, self.append))
        }
    }
}

/// Prepare the affixes needed to expand flags from the existing English dictionary.
fn english_affixes() -> Result<BTreeMap<char, Vec<EnglishAffix>>> {
    let mut rules = BTreeMap::<char, Vec<EnglishAffix>>::new();
    let mut groups = BTreeMap::new();
    for line in include_str!("../../assets/spelling/en_US.aff").lines() {
        let mut fields = line.split_whitespace();
        let before = match fields.next() {
            Some("PFX") => true,
            Some("SFX") => false,
            _ => continue,
        };
        let flag = fields
            .next()
            .and_then(|value| value.chars().next())
            .context("Missing spelling affix flag")?;
        let strip = fields.next().context("Missing spelling affix strip")?;
        let append = fields.next().context("Missing spelling affix text")?;
        let Some(condition) = fields.next() else {
            groups.insert((before, flag), strip == "Y");
            continue;
        };
        let condition = if before {
            format!("^(?:{condition})")
        } else {
            format!("(?:{condition})$")
        };
        rules.entry(flag).or_default().push(EnglishAffix {
            before,
            crosses: groups.get(&(before, flag)).copied().unwrap_or(false),
            strip: if strip == "0" { "" } else { strip },
            append: if append == "0" { "" } else { append },
            condition: regex::Regex::new(&condition).context("Invalid spelling affix condition")?,
        });
    }
    Ok(rules)
}

/// Prefix completion from the bundled spelling dictionary's headwords.
pub(super) fn dictionary_completion(prefix: &str) -> Vec<String> {
    include_str!("../../assets/spelling/en_US.dic")
        .lines()
        .skip(1)
        .map(|entry| {
            entry
                .split_once('/')
                .map_or(entry, |(headword, _)| headword)
        })
        .filter(|word| word.starts_with(prefix) && *word != prefix)
        .take(100)
        .map(str::to_owned)
        .collect()
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

/// Record the spelling `mark` for `word` in `list`, or delete it when `remove` is set.
/// Report whether the stored marking changed.
fn update_word(list: &mut WordList, word: &str, mark: SpellingMark, remove: bool) -> bool {
    if remove {
        list.remove(word).is_some()
    } else {
        list.insert(word.to_owned(), mark) != Some(mark)
    }
}

/// Change `word` in the numbered personal `index` at `path` without losing other writers.
/// `mark` selects the classification; `remove` deletes it instead of adding it.
fn update_personal_words(
    path: &Path,
    index: usize,
    word: &str,
    mark: SpellingMark,
    remove: bool,
) -> Result<()> {
    let _lock = app_data::lock_data_file(path)?;
    let mut lists = read_personal_words(path)?;
    let list = lists.entry(index).or_default();
    if !update_word(list, word, mark, remove) {
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
                .is_none_or(|(_, mark)| *mark != SpellingMark::Valid(false))
            && seen.insert(candidate.clone())
    });
    let lowered: Vec<_> = word.to_lowercase().chars().collect();
    for (key, (candidate, mark)) in &overrides {
        if *mark != SpellingMark::Valid(false)
            && similar(&lowered, key)
            && seen.insert(candidate.clone())
        {
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

    #[test]
    fn ex_spelling_editing_and_dump_use_the_same_internal_word_list() {
        let mut vim = Vim::new(crate::document::Document::new("nanobugexcommandlexeme"));
        let mut register = crate::vim::Register::default();
        let term = "nanobugexcommandlexeme";
        assert_eq!(
            vim.execute_ex_line(&format!("spellgood! {term}"), &mut register),
            Action::None
        );
        let dump = vim.execute_ex_line("spelldump", &mut register);
        assert_eq!(
            vim.execute_ex_line(&format!("spellundo! {term}"), &mut register),
            Action::None
        );
        assert!(
            matches!(dump, Action::EditableListing(words) if words.lines().any(|word| word == term) && words.lines().any(|word| word == "walked"))
        );
        assert_eq!(
            vim.execute_ex_line(&format!("spellwrong! {term}"), &mut register),
            Action::None
        );
        assert_eq!(
            SESSION_WORDS.lock().unwrap().get(term),
            Some(&SpellingMark::Valid(false))
        );
        assert_eq!(
            vim.execute_ex_line(&format!("spellundo! {term}"), &mut register),
            Action::None
        );
        assert!(!SESSION_WORDS.lock().unwrap().contains_key(term));
        assert_eq!(
            vim.execute_ex_line("spellinfo", &mut register),
            Action::None
        );
        assert!(vim.message.contains("en_US"));
    }
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

    /// Repeat a chosen suggestion for whole words and undo the whole batch together.
    #[test]
    fn spellrepall_uses_the_last_suggestion_for_whole_words() {
        let mut vim = Vim::new(Document::new("teh, xteh teh\nteh"));
        let mut register = Register::default();
        vim.text("z=", &mut register);
        let options = vim.spelling_suggestions().unwrap();
        let index = options.iter().position(|word| word == "the").unwrap();
        vim.key(
            &(SPELL_SUGGESTION_KEYS[index] as char).to_string(),
            &mut register,
        );
        vim.execute_ex_line("spellrepall", &mut register);
        assert_eq!(vim.doc.text, "the, xteh the\nthe");
        vim.key("u", &mut register);
        assert_eq!(vim.doc.text, "the, xteh teh\nteh");
    }

    /// A rare word remains in the personal dictionary as a separately reviewable classification.
    #[test]
    fn rare_spelling_updates_existing_boolean_history() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("spell.json");
        fs::write(&path, br#"{"1":{"example":true,"obscureword":false}}"#)?;
        update_personal_words(
            &path,
            1,
            "obscureword",
            SpellingMark::Rare(RareMark::Rare),
            false,
        )?;
        let list = read_personal_words(&path)?;
        assert_eq!(list[&1]["example"], SpellingMark::Valid(true));
        assert_eq!(list[&1]["obscureword"], SpellingMark::Rare(RareMark::Rare));
        Ok(())
    }

    /// Persist numbered lists and remove only markings in the requested list.
    #[test]
    fn personal_spell_lists_persist_independently() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("spell.json");
        update_personal_words(&path, 1, "Nanobug", SpellingMark::Valid(true), false)?;
        update_personal_words(&path, 2, "Nanobug", SpellingMark::Valid(false), false)?;
        update_personal_words(&path, 1, "Nanobug", SpellingMark::Valid(false), true)?;
        let lists = read_personal_words(&path)?;
        assert!(!lists.contains_key(&1));
        assert_eq!(lists[&2]["Nanobug"], SpellingMark::Valid(false));
        Ok(())
    }

    /// Uppercase spell commands share transient words across buffers and undo either marking.
    #[test]
    fn uppercase_spelling_marks_remain_in_the_session() {
        let mut vim = Vim::new(Document::new("nanobugtesttemporaryword\n"));
        let mut register = Register::default();
        let word = vim.doc.line(0).to_owned();
        vim.text("zG", &mut register);
        assert_eq!(
            SESSION_WORDS.lock().unwrap().get(&word),
            Some(&SpellingMark::Valid(true))
        );
        let mut vim = Vim::new(Document::new(&word));
        vim.text("zuW", &mut register);
        assert_eq!(SESSION_WORDS.lock().unwrap().get(&word), None);
        vim.text("zW", &mut register);
        assert_eq!(
            SESSION_WORDS.lock().unwrap().get(&word),
            Some(&SpellingMark::Valid(false))
        );
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
