//! User-defined modal key maps, language maps and text abbreviations.

mod config;
mod ex_verbose;
mod terminal;

pub use ex_verbose::{ExOrigin, ExOriginScope};
pub use terminal::TerminalStroke;

use super::{
    Action, Mode, Pending, Register, ScreenMotionResolver, Vim, VimOptions, replay::Stroke,
};
use crate::i18n;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use unicode_segmentation::UnicodeSegmentation;

/// The maximum number of keys in a mapped sequence or its rendered replacement.
const KEY_LIMIT: usize = 1024;
/// The number of recursive map expansions permitted while handling one physical key.
const RECURSION_LIMIT: usize = 32;

/// Global definitions shared by the editors in a workspace.
#[derive(Clone, Default)]
pub struct InputMappings(Arc<RwLock<Definitions>>);

/// Printable key shared by the workspace Leader bindings and user maps.
pub const LEADER_KEY: &str = " ";

/// Available mappings that extend the keys currently held by the modal parser.
pub struct PendingMappingHint {
    /// Keys already entered, with special keys shown in angle brackets.
    pub prefix: String,
    /// Remaining key sequences and the replacements they select.
    pub continuations: Vec<MappingContinuation>,
    /// Replacement for the current exact sequence when its input timeout expires.
    pub timeout_replacement: Option<String>,
}

/// One reachable mapped sequence shown after a pending custom prefix.
pub struct MappingContinuation {
    /// Keys still needed to select this replacement.
    pub keys: String,
    /// The right-hand side as defined, including expression or special key notation.
    pub replacement: String,
}

/// Active map table or abbreviation table selected by input context.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModeGroup {
    Language,
    Normal,
    Insert,
    Command,
    Terminal,
    Operator,
    Visual,
    Select,
    NormalVisualOperator,
    InsertCommand,
    VisualSelect,
    InsertAbbrev,
    CommandAbbrev,
    BothAbbrev,
}

impl ModeGroup {
    /// Concrete tables selected by a command that can configure several modes.
    fn targets(self) -> &'static [Self] {
        use ModeGroup::*;
        match self {
            Language => &[Language],
            Normal => &[Normal],
            Insert => &[Insert],
            Command => &[Command],
            Terminal => &[Terminal],
            Operator => &[Operator],
            Visual => &[Visual],
            Select => &[Select],
            NormalVisualOperator => &[Normal, Visual, Select, Operator],
            InsertCommand => &[Insert, Command],
            VisualSelect => &[Visual, Select],
            InsertAbbrev => &[InsertAbbrev],
            CommandAbbrev => &[CommandAbbrev],
            BothAbbrev => &[InsertAbbrev, CommandAbbrev],
        }
    }

    /// Whether this group specifies an abbreviation with a text left side.
    fn abbreviations(self) -> bool {
        matches!(
            self,
            Self::InsertAbbrev | Self::CommandAbbrev | Self::BothAbbrev
        )
    }
}

/// Replacement and execution policy for a defined mapping or abbreviation.
#[derive(Clone)]
struct Rule {
    /// Unparsed replacement, kept for listing and removal.
    replacement: String,
    /// Definition text and its original leader, available for editable mappings.
    source: Option<RuleSource>,
    /// Source location displayed by :verbose map or :verbose abbreviate.
    defined_at: Option<ExOrigin>,
    /// Allow key maps while treating the right side as typed keys.
    recursive: bool,
    /// Defer replacing the right side with the result of an expression.
    expression: bool,
    /// Use Visual commands and restore Select at the end of a shared Visual/Select mapping.
    restore_select: bool,
    /// Resolve this rule immediately even when its left side prefixes another map.
    nowait: bool,
    /// Newer definitions take precedence over same-scope partial matches for <nowait>.
    order: u64,
}

/// Capture the notation and leader at definition time so editing the global leader cannot rebind it.
#[derive(Clone)]
struct RuleSource {
    /// Original left-hand side, including escapes that need to survive a round trip.
    lhs: String,
    /// Value of `<Leader>` when the rule was defined.
    leader: String,
}

/// Selected keymap file and its language rules, initially shared with the editor defaults.
#[derive(Clone, Default)]
struct LoadedKeymap {
    /// The name passed to the keymap option.
    name: String,
    /// Project directory used to resolve a selected file on startup.
    directory: PathBuf,
    /// Short status label declared by the file.
    short_name: Option<String>,
    /// Language rules from the selected file.
    rules: Arc<BTreeMap<Vec<String>, Rule>>,
}

impl LoadedKeymap {
    /// Resolve `name` in `directory` and convert its file rows to language rules.
    fn load(directory: &Path, name: &str) -> Result<Self, super::keymap::Error> {
        if name.is_empty() {
            return Ok(Self::default());
        }
        let table = super::keymap::load(directory, name)?;
        Ok(Self::from_table(directory, name, table))
    }

    /// Keep a table's source identity and compile its local language mappings.
    fn from_table(directory: &Path, name: &str, table: super::keymap::Table) -> Self {
        let rules = table.rows.into_iter().map(|(left, replacement)| {
            (
                tokenize(&left),
                Rule {
                    replacement,
                    source: None,
                    defined_at: None,
                    recursive: true,
                    expression: false,
                    restore_select: false,
                    nowait: false,
                    order: 0,
                },
            )
        });
        Self {
            name: name.to_owned(),
            directory: directory.to_owned(),
            short_name: table.short_name,
            rules: Arc::new(rules.collect()),
        }
    }
}

/// Describe `error` while resolving a keymap called `name`.
fn keymap_error(name: &str, error: super::keymap::Error) -> String {
    use super::keymap::Error;
    match error {
        Error::Name => i18n::format("vim_mapping.keymap_invalid", &[("name", &name)]),
        Error::Missing => i18n::format("vim_mapping.keymap_missing", &[("name", &name)]),
        Error::File(error) => i18n::format(
            "vim_mapping.keymap_failed",
            &[("name", &name), ("error", &error)],
        ),
    }
}

/// Workspace defaults and global input tables.
#[derive(Default)]
struct Definitions {
    /// Script definitions and variables shared by the workspace's Ex interpreters.
    scripts: Arc<RwLock<super::script::Shared>>,
    /// Menu definitions shared across panes with keyboard mappings.
    menus: super::menu::Menus,
    /// Runtime digraph overrides shared by all buffers in the workspace.
    digraphs: BTreeMap<[char; 2], char>,
    /// Default keymap copied by buffers that enter editing for the first time.
    keymap: LoadedKeymap,
    /// Common options shared by the editors in a workspace.
    options: VimOptions,
    /// Verbosity overrides and the origin of temporarily executing commands.
    verbose: ex_verbose::State,
    /// Names and styles used by Ex and retained editor rendering.
    highlights: super::ex_highlight::Highlights,
    /// Sign types available to every buffer, with styles retained by old placements.
    signs: super::signs::SignDefinitions,
    /// Mappings declared without `<buffer>`.
    tables: MappingTables,
    /// Recent Ex messages shared by all editor windows in the workspace.
    messages: VecDeque<super::ExMessageOutput>,
    /// Highlight group used for the workspace's next echo operation.
    echo_highlight: Option<String>,
    /// Current Ex redirection, independent of the active editor.
    ex_redirection: Option<super::ex_redirect::Redirection>,
    /// Ex output already recorded by commands that emit more than one message.
    ex_output_sequence: u64,
    /// Finished clipboard redirection awaiting transfer by the workspace.
    redirected_clipboard: Option<String>,
    /// Overrides inherited by script commands dispatched inside a temporary Ex modifier.
    ex_message_modes: Vec<super::MessageModifier>,
    /// Latest Ex error text exposed through v:errmsg, including silent failures.
    ex_error: String,
    /// Source and result of the last applied z= suggestion in this workspace.
    spelling_replacement: Option<(String, String)>,
}

/// Concrete mappings and abbreviations in one global or buffer-local scope.
#[derive(Clone, Default)]
struct MappingTables {
    /// Definition order within this scope, shared across its modal tables.
    order: u64,
    language: BTreeMap<Vec<String>, Rule>,
    normal: BTreeMap<Vec<String>, Rule>,
    insert: BTreeMap<Vec<String>, Rule>,
    command: BTreeMap<Vec<String>, Rule>,
    terminal: BTreeMap<Vec<String>, Rule>,
    operator: BTreeMap<Vec<String>, Rule>,
    visual: BTreeMap<Vec<String>, Rule>,
    select: BTreeMap<Vec<String>, Rule>,
    insert_abbrev: BTreeMap<Vec<String>, Rule>,
    command_abbrev: BTreeMap<Vec<String>, Rule>,
}

impl InputMappings {
    /// Define or update a workspace sign type using the supplied fields.
    pub fn define_sign(
        &self,
        name: &str,
        update: super::SignDefinitionUpdate,
    ) -> Result<(), String> {
        self.0.write().unwrap().signs.define(name, update)
    }

    /// Get the current sign type, resolving a padded numeric name to the same definition.
    pub fn sign_definition(&self, name: &str) -> Option<super::SignDefinition> {
        self.0.read().unwrap().signs.get(name, false)
    }

    /// Resolve sign styles created earlier, including the style of a retired type.
    pub fn sign_render_definition(&self, name: &str) -> Option<super::SignDefinition> {
        self.0.read().unwrap().signs.get(name, true)
    }

    /// Return the current sign types in stable order for listing or completion.
    pub fn sign_definitions(&self) -> Vec<super::SignDefinition> {
        self.0.read().unwrap().signs.list()
    }

    /// Undefine a sign type, or all types when `name` is absent.
    /// Return false if a supplied name did not name an existing definition.
    pub fn undefine_sign(&self, name: Option<&str>) -> bool {
        self.0.write().unwrap().signs.undefine(name)
    }

    /// Refresh the default groups from the current native palette.
    pub fn set_highlight_palette(
        &self,
        palette: impl IntoIterator<Item = (&'static str, super::HighlightStyle)>,
    ) {
        self.0.write().unwrap().highlights.set_palette(palette);
    }

    /// Install default syntax group parents after updating the current palette.
    pub fn set_highlight_inheritance(&self, parents: Vec<(&'static str, &'static str)>) {
        self.0.write().unwrap().highlights.set_parents(parents);
    }

    /// Share the immutable syntax styles for the current palette and user settings.
    pub fn syntax_styles(&self) -> Arc<[Option<super::HighlightStyle>]> {
        self.0.read().unwrap().highlights.syntax_styles()
    }

    /// Resolve a known highlight group, following palette colors and Ex links.
    pub fn highlight_style(&self, group: &str) -> Option<super::HighlightStyle> {
        self.0.read().unwrap().highlights.style(group, false)
    }

    /// Serialize changes to the shared style registry with Ex commands and rendering queries.
    pub(super) fn edit_highlights<T>(
        &self,
        edit: impl FnOnce(&mut super::ex_highlight::Highlights) -> T,
    ) -> T {
        let mut maps = self.0.write().unwrap();
        maps.highlights.invalidate_syntax();
        edit(&mut maps.highlights)
    }

    /// Return an explicitly configured group style to overlay on a native theme role.
    pub fn highlight_override(&self, group: &str) -> Option<super::HighlightStyle> {
        self.0.read().unwrap().highlights.style(group, true)
    }

    /// Complete names matching a case-insensitive prefix.
    pub fn highlight_groups(&self, prefix: &str) -> Vec<String> {
        self.0.read().unwrap().highlights.names(prefix)
    }

    /// Apply one operation with the current Ex source and verbosity, without nesting registry locks.
    pub(super) fn apply_highlight(
        &self,
        command: super::ex_highlight::HighlightCommand,
    ) -> Result<Option<super::ExMessageOutput>, String> {
        let origin = self.current_ex_origin();
        let verbose = self.ex_verbose() > 0;
        self.0
            .write()
            .unwrap()
            .highlights
            .apply(command, origin, verbose)
    }

    /// Start capturing subsequent Ex messages into an open destination.
    pub(super) fn start_redirection(&self, target: super::ex_redirect::Redirection) {
        self.0.write().unwrap().ex_redirection = Some(target);
    }

    /// Close a destination and transfer collected output for finalization.
    pub(super) fn take_redirection(&self) -> Option<super::ex_redirect::Redirection> {
        self.0.write().unwrap().ex_redirection.take()
    }

    /// Start capturing to a file using the workspace-resolved path.
    pub fn begin_redirect_file(
        &self,
        path: &std::path::Path,
        mode: super::RedirectMode,
    ) -> std::io::Result<()> {
        self.start_redirection(super::ex_redirect::Redirection::file(path, mode)?);
        Ok(())
    }

    /// A clipboard append starts with the host's current clipboard contents.
    pub fn begin_redirect_clipboard(&self, existing: String) {
        self.start_redirection(super::ex_redirect::Redirection::Clipboard(existing));
    }

    /// Transfer a completed capture to the native clipboard on the next editor action.
    pub(super) fn finish_redirect_clipboard(&self, text: String) {
        self.0.write().unwrap().redirected_clipboard = Some(text);
    }

    /// Drain text written to the clipboard by a completed Ex redirection.
    pub fn take_redirected_clipboard(&self) -> Option<String> {
        self.0.write().unwrap().redirected_clipboard.take()
    }

    /// Send output to the active redirection without feeding message history or verbose logging.
    pub fn redirect_ex_output(&self, text: &str, newline: bool) {
        if let Some(target) = self.0.write().unwrap().ex_redirection.as_mut() {
            target.write(text, newline);
        }
    }

    /// Send a rendered Ex message to active redirection and verbose output independently of display.
    pub fn record_ex_output(&self, text: &str, newline: bool) {
        self.redirect_ex_output(text, newline);
        self.append_ex_verbosefile(text, newline);
        let mut global = self.0.write().unwrap();
        global.ex_output_sequence = global.ex_output_sequence.wrapping_add(1);
    }

    /// Detect whether a nested command already emitted its own output.
    pub(super) fn ex_output_sequence(&self) -> u64 {
        self.0.read().unwrap().ex_output_sequence
    }

    /// Whether a locally interpreted listing has a destination without a native view.
    pub(super) fn capturing_ex_output(&self) -> bool {
        let global = self.0.read().unwrap();
        global.ex_redirection.is_some() || !global.options.verbosefile.is_empty()
    }

    /// Remember `source` and `result` so :spellrepall can repeat the replacement in any window.
    pub(super) fn set_spelling_replacement(&self, source: String, result: String) {
        self.0.write().unwrap().spelling_replacement = Some((source, result));
    }

    /// Return the previous suggestion pair for this editor workspace.
    pub(super) fn spelling_replacement(&self) -> Option<(String, String)> {
        self.0.read().unwrap().spelling_replacement.clone()
    }

    /// Share Ex data without holding the input-mappings lock during script execution.
    pub(super) fn shared_scripts(&self) -> Arc<RwLock<super::script::Shared>> {
        self.0.read().unwrap().scripts.clone()
    }

    /// Keep one nonempty Ex message for subsequent `:messages` queries.
    pub fn remember_ex_message(&self, message: &str) {
        self.remember_styled_ex_message(super::ExMessageOutput {
            text: message.into(),
            spans: Vec::new(),
        });
    }

    /// Keep the styles on a history entry after :echohl changes the active group.
    pub(super) fn remember_styled_ex_message(&self, output: super::ExMessageOutput) {
        if output.text.is_empty() {
            return;
        }
        let mut global = self.0.write().unwrap();
        if global
            .ex_message_modes
            .iter()
            .rev()
            .find(|mode| !matches!(mode, super::MessageModifier::Filter { .. }))
            .is_some_and(|mode| matches!(mode, super::MessageModifier::Silent { .. }))
        {
            return;
        }
        if global.messages.len() == 500 {
            global.messages.pop_front();
        }
        global.messages.push_back(output);
    }

    /// Select an existing group, with None or an unknown group restoring ordinary messages.
    pub(super) fn set_echo_highlight(&self, group: &str) {
        let selected = (!group.eq_ignore_ascii_case("None")
            && self.highlight_style(group).is_some())
        .then(|| group.to_owned());
        self.0.write().unwrap().echo_highlight = selected;
    }

    /// Resolve the currently selected group when its next message is written.
    pub(super) fn echo_highlight(&self) -> Option<super::HighlightStyle> {
        let group = self.0.read().unwrap().echo_highlight.clone()?;
        self.highlight_style(&group)
    }

    /// Push a scoped output policy before executing its nested command.
    pub fn begin_ex_message_modifier(&self, modifier: super::MessageModifier) {
        self.0.write().unwrap().ex_message_modes.push(modifier);
    }

    /// Restore the containing command's output policy.
    pub fn end_ex_message_modifier(&self) {
        self.0.write().unwrap().ex_message_modes.pop();
    }

    /// Obtain the effective silence mode; a filter leaves visibility unchanged.
    pub fn ex_silence(&self) -> Option<bool> {
        self.0
            .read()
            .unwrap()
            .ex_message_modes
            .iter()
            .rev()
            .find_map(|mode| match mode {
                super::MessageModifier::Silent { ignore_error } => Some(Some(*ignore_error)),
                super::MessageModifier::Unsilent => Some(None),
                super::MessageModifier::Filter { .. } => None,
            })
            .flatten()
    }

    /// Apply any enclosing output filters, starting with the most local one.
    pub fn filter_ex_output(&self, text: &str) -> String {
        self.0
            .read()
            .unwrap()
            .ex_message_modes
            .iter()
            .rev()
            .fold(text.to_owned(), |text, mode| mode.filter_text(&text))
    }

    /// Run filters on message text and keep the matching ranges' original styles.
    pub fn filter_styled_ex_output(
        &self,
        output: super::ExMessageOutput,
    ) -> super::ExMessageOutput {
        self.0
            .read()
            .unwrap()
            .ex_message_modes
            .iter()
            .rev()
            .fold(output, |output, mode| mode.filter_styled_output(output))
    }

    /// Retain the latest error for a Vim expression after an ignored command failure.
    pub fn remember_ex_error(&self, error: &str) {
        self.0.write().unwrap().ex_error = error.into();
    }

    /// Read the latest error for v:errmsg.
    pub(super) fn last_ex_error(&self) -> String {
        self.0.read().unwrap().ex_error.clone()
    }

    /// List the last `count` messages, or clear the history while keeping that many messages.
    pub(super) fn ex_messages(&self, count: Option<usize>, clear: bool) -> super::ExMessageOutput {
        if clear {
            let mut global = self.0.write().unwrap();
            let drop = global.messages.len().saturating_sub(count.unwrap_or(0));
            global.messages.drain(..drop);
            return super::ExMessageOutput::default();
        }
        let global = self.0.read().unwrap();
        let skip = count.map_or(0, |count| global.messages.len().saturating_sub(count));
        let mut output = super::ExMessageOutput::default();
        for record in global.messages.iter().skip(skip) {
            if !output.text.is_empty() {
                output.text.push('\n');
            }
            output.append_range(record, 0..record.text.len());
        }
        output
    }

    /// Inspect runtime digraphs while leaving insert input state in its buffer.
    pub(super) fn digraphs<T>(&self, read: impl FnOnce(&BTreeMap<[char; 2], char>) -> T) -> T {
        read(&self.0.read().unwrap().digraphs)
    }

    /// Install the validated `definitions` together.
    pub(super) fn define_digraphs(&self, definitions: impl IntoIterator<Item = ([char; 2], char)>) {
        self.0.write().unwrap().digraphs.extend(definitions);
    }

    /// Whether `other` shares the same live workspace definitions.
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Snapshot the current shared editing options.
    pub fn options(&self) -> VimOptions {
        self.0.read().unwrap().options.clone()
    }

    /// Inspect the current options without cloning strings for read-only queries.
    pub(super) fn with_options<T>(&self, read: impl FnOnce(&VimOptions) -> T) -> T {
        read(&self.0.read().unwrap().options)
    }

    /// Publish validated `options` to the workspace's current and future editors.
    pub fn set_options(&self, options: VimOptions) {
        self.0.write().unwrap().options = options;
    }

    /// Read the shared menu tree without holding the lock during execution.
    pub(super) fn menus<T>(&self, read: impl FnOnce(&super::menu::Menus) -> T) -> T {
        read(&self.0.read().unwrap().menus)
    }

    /// Apply one Ex definition or deletion to the shared menu tree.
    pub(super) fn change_menus<T>(&self, write: impl FnOnce(&mut super::menu::Menus) -> T) -> T {
        write(&mut self.0.write().unwrap().menus)
    }
}

impl MappingTables {
    /// Borrow the concrete table for `mode`.
    fn table(&self, mode: ModeGroup) -> &BTreeMap<Vec<String>, Rule> {
        match mode {
            ModeGroup::Language => &self.language,
            ModeGroup::Normal => &self.normal,
            ModeGroup::Insert => &self.insert,
            ModeGroup::Command => &self.command,
            ModeGroup::Terminal => &self.terminal,
            ModeGroup::Operator => &self.operator,
            ModeGroup::Visual => &self.visual,
            ModeGroup::Select => &self.select,
            ModeGroup::InsertAbbrev => &self.insert_abbrev,
            ModeGroup::CommandAbbrev => &self.command_abbrev,
            _ => unreachable!("take a concrete input mapping table"),
        }
    }

    /// Mutate the concrete table for `mode`.
    fn table_mut(&mut self, mode: ModeGroup) -> &mut BTreeMap<Vec<String>, Rule> {
        match mode {
            ModeGroup::Language => &mut self.language,
            ModeGroup::Normal => &mut self.normal,
            ModeGroup::Insert => &mut self.insert,
            ModeGroup::Command => &mut self.command,
            ModeGroup::Terminal => &mut self.terminal,
            ModeGroup::Operator => &mut self.operator,
            ModeGroup::Visual => &mut self.visual,
            ModeGroup::Select => &mut self.select,
            ModeGroup::InsertAbbrev => &mut self.insert_abbrev,
            ModeGroup::CommandAbbrev => &mut self.command_abbrev,
            _ => unreachable!("mutate a concrete input mapping table"),
        }
    }
}

/// Per-buffer options and temporary input matching state.
#[derive(Default)]
pub(super) struct State {
    /// Definitions declared with `<buffer>`, shared by all views of this buffer.
    local: Arc<RwLock<MappingTables>>,
    /// Buffer-local selection, absent until the buffer receives its workspace defaults.
    keymap: Option<LoadedKeymap>,
    /// Whether Insert and character arguments use language maps.
    insert_language: bool,
    /// Search override. None follows the Insert option.
    search_language: Option<bool>,
    /// Language mapping is off at the start of each non-search prompt.
    command_language: bool,
    /// Prefix received while matching multi-key left sides.
    held: Vec<String>,
    /// Terminal mode's committed input awaiting a longer custom sequence.
    terminal_held: Vec<String>,
    /// Whether a held prefix can still use language maps during timeout resolution.
    held_from: InputSource,
    /// Identity of the held prefix, changing each time another key is consumed.
    serial: u64,
    /// Active recursive expansion depth.
    depth: usize,
    /// Whether keys originate from a non-remappable RHS or an abbreviation.
    suppress_abbreviation: bool,
    /// A modal mapping already recorded its original left side in the running macro.
    pub(super) suppress_macro_input: bool,
    /// Suppress automatically reselecting a region left by a Select mapping.
    suppress_reselect: bool,
}

/// Origin determines whether a key can use language maps or modal mappings.
#[derive(Clone, Copy, Default)]
enum InputSource {
    #[default]
    Typed,
    Replacement,
    Script,
}

/// Resolve a queue in its input mode, optionally waiting for additional keys.
#[derive(Clone, Copy)]
struct InputOptions {
    /// Input origin, controlling translation and recording.
    source: InputSource,
    /// Whether an unresolved prefix should wait for another key.
    allow_wait: bool,
}

impl State {
    /// Whether a mapping owns the next key.
    pub(super) fn has_prefix(&self) -> bool {
        !self.held.is_empty()
    }

    /// Reset input matching at a buffer or pane boundary.
    pub(super) fn clear_prefix(&mut self) {
        self.held.clear();
        self.terminal_held.clear();
    }
}

/// Operation specified in a mapping or abbreviation Ex command.
#[derive(Clone, Copy)]
enum Operation {
    Define(bool),
    Remove,
    Clear,
}

/// Mapping spellings with their shortest accepted spelling, mode and operation.
const COMMANDS: &[(&str, usize, ModeGroup, Operation)] = {
    use ModeGroup::*;
    use Operation::*;
    &[
        ("mapclear", 4, NormalVisualOperator, Clear),
        ("noremap", 2, NormalVisualOperator, Define(false)),
        ("unmap", 3, NormalVisualOperator, Remove),
        ("map", 3, NormalVisualOperator, Define(true)),
        ("nmapclear", 5, Normal, Clear),
        ("nnoremap", 2, Normal, Define(false)),
        ("nunmap", 3, Normal, Remove),
        ("nmap", 2, Normal, Define(true)),
        ("imapclear", 5, Insert, Clear),
        ("inoremap", 3, Insert, Define(false)),
        ("iunmap", 2, Insert, Remove),
        ("imap", 2, Insert, Define(true)),
        ("cmapclear", 5, Command, Clear),
        ("cnoremap", 3, Command, Define(false)),
        ("cunmap", 2, Command, Remove),
        ("cmap", 2, Command, Define(true)),
        ("tmapclear", 5, Terminal, Clear),
        ("tnoremap", 3, Terminal, Define(false)),
        ("tunmap", 3, Terminal, Remove),
        ("tmap", 3, Terminal, Define(true)),
        ("omapclear", 5, Operator, Clear),
        ("onoremap", 3, Operator, Define(false)),
        ("ounmap", 2, Operator, Remove),
        ("omap", 2, Operator, Define(true)),
        ("lmapclear", 5, Language, Clear),
        ("lnoremap", 2, Language, Define(false)),
        ("lunmap", 2, Language, Remove),
        ("lmap", 2, Language, Define(true)),
        ("vmapclear", 5, VisualSelect, Clear),
        ("vnoremap", 2, VisualSelect, Define(false)),
        ("vunmap", 2, VisualSelect, Remove),
        ("vmap", 2, VisualSelect, Define(true)),
        ("xmapclear", 5, Visual, Clear),
        ("xnoremap", 2, Visual, Define(false)),
        ("xunmap", 2, Visual, Remove),
        ("xmap", 2, Visual, Define(true)),
        ("smapclear", 5, Select, Clear),
        ("snoremap", 4, Select, Define(false)),
        ("sunmap", 4, Select, Remove),
        ("smap", 3, Select, Define(true)),
        ("abclear", 3, BothAbbrev, Clear),
        ("noreabbrev", 5, BothAbbrev, Define(false)),
        ("unabbreviate", 3, BothAbbrev, Remove),
        ("abbreviate", 2, BothAbbrev, Define(true)),
        ("iabclear", 4, InsertAbbrev, Clear),
        ("inoreabbrev", 6, InsertAbbrev, Define(false)),
        ("iunabbrev", 4, InsertAbbrev, Remove),
        ("iabbrev", 2, InsertAbbrev, Define(true)),
        ("cabclear", 4, CommandAbbrev, Clear),
        ("cnoreabbrev", 6, CommandAbbrev, Define(false)),
        ("cunabbrev", 4, CommandAbbrev, Remove),
        ("cabbrev", 2, CommandAbbrev, Define(true)),
    ]
};

/// Return the Ex mapping command names starting with `prefix`.
pub(super) fn ex_candidates(prefix: &str) -> impl Iterator<Item = &'static str> + '_ {
    COMMANDS
        .iter()
        .map(|(name, ..)| *name)
        .chain(["map!", "noremap!", "unmap!", "mapclear!"])
        .filter(move |name| name.starts_with(prefix))
}

impl Vim {
    /// Install language mappings from a sourced `loadkeymap` directive in the current buffer.
    pub fn load_ex_keymap(&mut self, source: &str) -> bool {
        match super::keymap::sourced_keymap(source) {
            Ok(table) => {
                let name = self
                    .input_maps
                    .keymap
                    .as_ref()
                    .map_or("", |keymap| keymap.name.as_str());
                let keymap = LoadedKeymap::from_table(self.completion_directory(), name, table);
                self.input_maps.keymap = Some(keymap);
                self.input_maps.clear_prefix();
                self.clear_ex_message();
                self.ex_failed = false;
                true
            }
            Err(error) => {
                self.ex_failed = true;
                self.message = keymap_error("loadkeymap", error);
                false
            }
        }
    }

    /// Use the same buffer-local definitions as `source`, retaining this view's input state.
    pub(super) fn share_buffer_mappings(&mut self, source: &Self) {
        self.input_maps.local.clone_from(&source.input_maps.local);
    }

    /// Copy buffer-local definitions when a buffer leaves its Session.
    pub(super) fn detach_buffer_mappings(&mut self) {
        let local = self.input_maps.local.read().unwrap().clone();
        self.input_maps.local = Arc::new(RwLock::new(local));
    }

    /// Initialize a new buffer from the binding defaults shared by its `register`.
    fn inherit_keymap(&mut self, register: &Register) {
        if self.input_maps.keymap.is_none() {
            let keymap = register.input_maps.0.read().unwrap().keymap.clone();
            if !keymap.name.is_empty() {
                self.input_maps.insert_language = true;
            }
            self.input_maps.keymap = Some(keymap);
        }
    }

    /// Return the selected keymap's status label while this buffer is taking mapped input.
    pub fn active_keymap_label(&self) -> Option<&str> {
        let preset = self.input_maps.keymap.as_ref()?;
        (self.accepts_text() && self.use_language_map() && !preset.name.is_empty())
            .then(|| preset.short_name.as_deref().unwrap_or(&preset.name))
    }

    /// Whether the native editor should consume `key` as a mapped modifier chord.
    pub fn accepts_language_chord(&self, key: &str, register: &Register) -> bool {
        if !self.use_language_key(key, InputSource::Typed) {
            return false;
        }
        let groups = [Some(ModeGroup::Language), None];
        self.mapping_continues(groups, key, register)
            || self.mapping_matches(groups, &[key.to_owned()], register)
    }

    /// Whether this native `key` starts or continues an eligible user mapping.
    pub fn accepts_input_mapping(&self, key: &str, register: &Register) -> bool {
        let first = self.input_maps.held.first().map_or(key, String::as_str);
        self.mapping_continues(self.input_groups(first, InputSource::Typed), key, register)
    }

    /// Return an unresolved shell sequence when the workspace can resume its built-in routing.
    pub fn take_unmapped_shell_prefix(&mut self, register: &Register) -> Option<Vec<String>> {
        let keys = &self.input_maps.held;
        let first = keys.first()?;
        if self.mode.accepts_text()
            || self.pending.is_some()
            || !matches!(first.as_str(), LEADER_KEY | "\\" | "Ctrl+w")
        {
            return None;
        }
        let groups = self.input_groups(first, self.input_maps.held_from);
        let (_, matched, _) = self.match_mapping(groups, keys, register);
        matched
            .is_none()
            .then(|| std::mem::take(&mut self.input_maps.held))
    }

    /// Test a native key together with the currently held sequence.
    fn mapping_continues(
        &self,
        groups: [Option<ModeGroup>; 2],
        key: &str,
        register: &Register,
    ) -> bool {
        let mut keys = self.input_maps.held.clone();
        keys.push(key.to_owned());
        self.mapping_matches(groups, &keys, register)
    }

    /// Whether the entire sequence can match, even if resolving it requires another key.
    fn mapping_matches(
        &self,
        groups: [Option<ModeGroup>; 2],
        keys: &[String],
        register: &Register,
    ) -> bool {
        let (len, _, waiting) = self.match_mapping(groups, keys, register);
        len == keys.len() || waiting
    }

    /// Query, change, or clear a keymap option in `args` through the selected Ex `command`.
    fn configure_keymap(&mut self, command: &str, args: &str, register: &Register) -> bool {
        let Some(value) = args
            .strip_prefix("keymap")
            .or_else(|| args.strip_prefix("kmp"))
        else {
            return false;
        };
        let only_global = matches!(command, "setglobal" | "setg");
        let only_local = matches!(command, "setlocal" | "setl");
        let name = match value {
            "" | "?" => {
                let name = if only_global {
                    register.input_maps.0.read().unwrap().keymap.name.clone()
                } else {
                    self.input_maps
                        .keymap
                        .as_ref()
                        .map_or_else(String::new, |keymap| keymap.name.clone())
                };
                self.message = format!("keymap={name}");
                return true;
            }
            "&" | "&vim" | "&vi" => "",
            value if value.starts_with('=') => &value[1..],
            _ => return false,
        };
        let keymap = match LoadedKeymap::load(self.completion_directory(), name) {
            Ok(keymap) => keymap,
            Err(error) => {
                self.message = keymap_error(name, error);
                return true;
            }
        };
        if !only_local {
            register.input_maps.0.write().unwrap().keymap = keymap.clone();
        }
        if !only_global {
            let active = !keymap.name.is_empty();
            self.input_maps.insert_language = active;
            if !active {
                self.input_maps.search_language = None;
            } else if self.input_maps.search_language.is_some() {
                self.input_maps.search_language = Some(true);
            }
            self.input_maps.keymap = Some(keymap);
            self.input_maps.clear_prefix();
        }
        true
    }

    /// Consume Ex mapping/options `text`, editing the buffer or shared `register` definitions.
    pub(super) fn configure_input(&mut self, text: &str, register: &Register) -> bool {
        self.inherit_keymap(register);
        let text = text.trim_start();
        let (name, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let mut args = args.trim_start();
        if matches!(
            name,
            "set" | "setlocal" | "setglobal" | "se" | "setl" | "setg"
        ) {
            if self.configure_keymap(name, args.trim_end(), register) {
                return true;
            }
            if matches!(name, "setglobal" | "setg") {
                return false;
            }
            match args.trim_end() {
                "iminsert=0" | "imi=0" => self.input_maps.insert_language = false,
                "iminsert=1" | "imi=1" => self.input_maps.insert_language = true,
                "imsearch=-1" | "ims=-1" => self.input_maps.search_language = None,
                "imsearch=0" | "ims=0" => self.input_maps.search_language = Some(false),
                "imsearch=1" | "ims=1" => self.input_maps.search_language = Some(true),
                "iminsert?" | "imi?" => {
                    self.message = format!("iminsert={}", u8::from(self.input_maps.insert_language))
                }
                "imsearch?" | "ims?" => {
                    self.message = format!(
                        "imsearch={}",
                        self.input_maps.search_language.map_or(-1, i8::from)
                    )
                }
                _ => return false,
            }
            return true;
        }
        let (name, bang) = name
            .strip_suffix('!')
            .map_or((name, false), |name| (name, true));
        let Some(&(_, _, mut mode, op)) = COMMANDS
            .iter()
            .find(|(full, min, _, _)| name.len() >= *min && full.starts_with(name))
        else {
            return false;
        };
        if bang {
            if mode != ModeGroup::NormalVisualOperator {
                return false;
            }
            mode = ModeGroup::InsertCommand;
        }
        let mut local = false;
        let mut expression = false;
        let mut nowait = false;
        while let Some((flag, more)) = args
            .split_once(char::is_whitespace)
            .or_else(|| args.starts_with('<').then_some((args, "")))
        {
            match flag.to_ascii_lowercase().as_str() {
                "<buffer>" => local = true,
                "<expr>" => expression = true,
                "<nowait>" => nowait = true,
                "<silent>" => {}
                _ => break,
            }
            args = more.trim_start();
        }
        let (left, right) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let replacement = right.trim_start();
        if matches!(op, Operation::Clear) && !left.is_empty()
            || matches!(op, Operation::Remove) && left.is_empty()
            || mode.abbreviations() && !replacement.is_empty() && abbreviation_kind(left).is_none()
        {
            self.message = i18n::text("vim_mapping.invalid").into();
            return true;
        }
        let leader = register.input_maps.options().mapleader;
        let lhs = mapping_lhs(mode, left, &leader);
        if left.len() > KEY_LIMIT
            || (!left.is_empty() && lhs.is_empty())
            || replacement.len() > KEY_LIMIT * 8
        {
            self.message = i18n::text("vim_mapping.invalid").into();
            return true;
        }
        if matches!(op, Operation::Define(_)) && replacement.is_empty() {
            self.list_mappings(mode, local, &lhs, register);
            return true;
        }

        let defined_at = register.input_maps.current_ex_origin();
        let mut shared;
        let mut scoped;
        let definitions = if local {
            scoped = self.input_maps.local.write().unwrap();
            &mut *scoped
        } else {
            shared = register.input_maps.0.write().unwrap();
            &mut shared.tables
        };
        if matches!(op, Operation::Define(_)) {
            definitions.order += 1;
        }
        let order = definitions.order;
        let mut preset = self
            .input_maps
            .keymap
            .as_mut()
            .filter(|_| mode == ModeGroup::Language);
        for &target in mode.targets() {
            let table = definitions.table_mut(target);
            match op {
                Operation::Clear => {
                    table.clear();
                    if local && let Some(preset) = preset.as_deref_mut() {
                        Arc::make_mut(&mut preset.rules).clear();
                    }
                }
                Operation::Remove => {
                    if table.remove(&lhs).is_none() {
                        table.retain(|_, rule| rule.replacement != args);
                        if local && let Some(preset) = preset.as_deref_mut() {
                            let rules = Arc::make_mut(&mut preset.rules);
                            if rules.remove(&lhs).is_none() {
                                rules.retain(|_, rule| rule.replacement != args);
                            }
                        }
                    }
                }
                Operation::Define(recursive) => {
                    table.insert(
                        lhs.clone(),
                        Rule {
                            replacement: replacement.to_owned(),
                            source: Some(RuleSource {
                                lhs: left.to_owned(),
                                leader: leader.clone(),
                            }),
                            defined_at: defined_at.clone(),
                            recursive,
                            expression,
                            restore_select: matches!(
                                mode,
                                ModeGroup::VisualSelect | ModeGroup::NormalVisualOperator
                            ),
                            nowait,
                            order,
                        },
                    );
                }
            }
        }
        true
    }

    /// List matching definitions from this buffer and, unless `local_only`, the global scope.
    fn list_mappings(
        &mut self,
        mode: ModeGroup,
        local_only: bool,
        prefix: &[String],
        register: &Register,
    ) {
        let mut output = Vec::new();
        let verbose = register.input_maps.ex_verbose() > 0;
        let mut append = |table: &BTreeMap<Vec<String>, Rule>, scope| {
            for (keys, rule) in table {
                let matches = if mode.abbreviations() {
                    prefix.first().is_none_or(|text| keys[0].starts_with(text))
                } else {
                    keys.starts_with(prefix)
                };
                if matches {
                    let lhs = if mode.abbreviations() {
                        keys[0].clone()
                    } else {
                        display_mapping_keys(keys)
                    };
                    let mut entry = format!("{lhs}  {scope}{}", rule.replacement);
                    if verbose && let Some(origin) = &rule.defined_at {
                        origin.append_inline(&mut entry);
                    }
                    output.push(entry);
                }
            }
        };
        let global = register.input_maps.0.read().unwrap();
        let local = self.input_maps.local.read().unwrap();
        for &target in mode.targets() {
            append(local.table(target), "@");
            if !local_only {
                append(global.tables.table(target), "");
            }
        }
        if mode == ModeGroup::Language
            && let Some(preset) = &self.input_maps.keymap
        {
            append(&preset.rules, "@");
        }
        output.sort_unstable();
        output.dedup();
        self.message = if output.is_empty() {
            i18n::text("vim_mapping.no_rules").into()
        } else {
            output.join("  ·  ")
        };
    }

    /// Begin a fresh non-search command line with Insert's language map temporarily disabled.
    pub(super) fn begin_input_prompt(&mut self) {
        self.input_maps.command_language = false;
    }

    /// Whether a text field or literal argument uses the buffer's language definitions.
    fn use_language_map(&self) -> bool {
        if self.mode == Mode::Search {
            self.input_maps
                .search_language
                .unwrap_or(self.input_maps.insert_language)
        } else if self.mode == Mode::Command {
            self.input_maps.command_language
        } else {
            self.input_maps.insert_language
        }
    }

    /// Toggle language mapping for the current text mode without changing the system IME.
    pub(super) fn toggle_language(&mut self) {
        let next = !self.use_language_map();
        match self.mode {
            Mode::Search => self.input_maps.search_language = Some(next),
            Mode::Command => self.input_maps.command_language = next,
            _ => self.input_maps.insert_language = next,
        }
        self.message = i18n::text(if next {
            "vim_mapping.language_on"
        } else {
            "vim_mapping.language_off"
        })
        .into();
    }

    /// Whether typed or committed input can use language maps at the current position.
    fn use_language_key(&self, key: &str, source: InputSource) -> bool {
        matches!(source, InputSource::Typed)
            && !matches!(key, "Escape" | "Ctrl+[" | "Ctrl+c" | "Ctrl+6" | "Ctrl+^")
            && self.use_language_map()
            && (self.mode.accepts_text() && self.pending.is_none()
                || self.awaits_literal_argument()
                    && !matches!(self.pending, Some(Pending::InsertArgument(_))))
    }

    /// Select eligible tables for the next key, giving language maps equal-length priority.
    fn input_groups(&self, key: &str, source: InputSource) -> [Option<ModeGroup>; 2] {
        if self.substitute_confirmation_active() {
            return [None, None];
        }
        let modal = match self.pending {
            // A zero after a count extends it, independent of any map for the 0 motion.
            _ if key == "0" && self.count.is_some() => None,
            Some(Pending::Operator(_)) => Some(ModeGroup::Operator),
            Some(_) => None,
            None => Some(match self.mode {
                Mode::Normal => ModeGroup::Normal,
                Mode::Insert | Mode::Replace => ModeGroup::Insert,
                Mode::Command | Mode::Search => ModeGroup::Command,
                _ if self.g.select => ModeGroup::Select,
                _ => ModeGroup::Visual,
            }),
        };
        [
            self.use_language_key(key, source)
                .then_some(ModeGroup::Language),
            modal,
        ]
    }

    /// Apply a `key` typed by the user, using `register` and `resolve_screen` for its effects.
    pub(super) fn dispatch_typed(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.inherit_keymap(register);
        self.apply_mappings(key, register, resolve_screen, InputSource::Typed)
    }

    /// Replay register keys through the current modal maps, after their original language translation.
    pub(super) fn dispatch_macro_key(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.apply_mappings(key, register, resolve_screen, InputSource::Replacement)
    }

    /// Resolve a complete `:normal` key sequence within the enclosing Ex command.
    pub(super) fn dispatch_mapped_script_keys(
        &mut self,
        keys: Vec<String>,
        register: &mut Register,
    ) -> Action {
        self.run_input_queue(
            keys,
            register,
            &mut |_, _, _, _| None,
            InputOptions {
                source: InputSource::Script,
                allow_wait: false,
            },
        )
    }

    /// Check whether `text` needs individual keys for mappings or abbreviations.
    pub(super) fn map_committed_text(&mut self, text: &str, register: &Register) -> bool {
        self.inherit_keymap(register);
        let global = register.input_maps.0.read().unwrap();
        let global = &global.tables;
        let local = self.input_maps.local.read().unwrap();
        self.input_maps.has_prefix()
            || self
                .input_groups(text, InputSource::Typed)
                .into_iter()
                .flatten()
                .any(|mode| {
                    !global.table(mode).is_empty()
                        || !local.table(mode).is_empty()
                        || mode == ModeGroup::Language
                            && self
                                .input_maps
                                .keymap
                                .as_ref()
                                .is_some_and(|preset| !preset.rules.is_empty())
                })
            || self.mode.is_insert()
                && (!local.insert_abbrev.is_empty() || !global.insert_abbrev.is_empty())
            || self.mode == Mode::Command
                && (!local.command_abbrev.is_empty() || !global.command_abbrev.is_empty())
    }

    /// Add `key` to any held prefix and dispatch through the current map tables.
    fn apply_mappings(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
        source: InputSource,
    ) -> Action {
        let mut keys = std::mem::take(&mut self.input_maps.held);
        keys.push(key.into());
        self.run_input_queue(
            keys,
            register,
            resolve_screen,
            InputOptions {
                source,
                allow_wait: true,
            },
        )
    }

    /// Resolve a queued sequence again whenever a literal key or mapping changes its mode.
    fn run_input_queue(
        &mut self,
        mut keys: Vec<String>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
        options: InputOptions,
    ) -> Action {
        while !keys.is_empty() {
            let groups = self.input_groups(&keys[0], options.source);
            let (len, matched, waiting) = self.match_mapping(groups, &keys, register);
            if waiting && options.allow_wait {
                self.input_maps.held = keys;
                self.input_maps.held_from = options.source;
                self.input_maps.serial = self.input_maps.serial.wrapping_add(1);
                return Action::None;
            }
            let action = if let Some((mode, rule)) = matched {
                let lhs = &keys[..len];
                if mode != ModeGroup::Language && !matches!(options.source, InputSource::Script) {
                    self.capture_macro_input(lhs.iter().cloned().map(Stroke::Key), register);
                }
                let action = self.run_mapping(&rule, mode, lhs, register, resolve_screen, options);
                keys.drain(..len);
                action
            } else {
                self.dispatch_unmapped_key(keys.remove(0), register, resolve_screen, options.source)
            };
            if action != Action::None {
                return action;
            }
            if !keys.is_empty() && self.input_maps.has_prefix() {
                self.input_maps.held.append(&mut keys);
                keys = std::mem::take(&mut self.input_maps.held);
            }
        }
        Action::None
    }

    /// Let the enclosing Ex command record and track the effects of its own fed keys.
    fn dispatch_unmapped_key(
        &mut self,
        key: String,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
        source: InputSource,
    ) -> Action {
        if matches!(source, InputSource::Script) {
            self.handle_key(&key, register, resolve_screen)
        } else {
            self.dispatch(Stroke::Key(key), register, resolve_screen)
        }
    }

    /// Resolve the longest exact left side in `keys`, or wait if the prefix can still grow.
    fn match_mapping(
        &self,
        groups: [Option<ModeGroup>; 2],
        keys: &[String],
        register: &Register,
    ) -> (usize, Option<(ModeGroup, Rule)>, bool) {
        if groups.iter().all(Option::is_none) {
            return (0, None, false);
        }
        let keys = canonical_input(keys);
        let global = register.input_maps.0.read().unwrap();
        let local = self.input_maps.local.read().unwrap();
        let (exact, waiting) = self.match_in_tables(groups, &keys, &local, &global.tables);
        exact.map_or((0, None, waiting), |(len, mode, rule)| {
            (len, Some((mode, rule.clone())), waiting)
        })
    }

    /// Iterate the eligible tables in the precedence order used for equal-length matches.
    fn mapping_tables<'a>(
        &'a self,
        groups: [Option<ModeGroup>; 2],
        local: &'a MappingTables,
        global: &'a MappingTables,
    ) -> impl Iterator<Item = (ModeGroup, &'a BTreeMap<Vec<String>, Rule>)> {
        groups.into_iter().flatten().flat_map(move |mode| {
            let preset = self
                .input_maps
                .keymap
                .as_ref()
                .filter(|_| mode == ModeGroup::Language)
                .map(|preset| preset.rules.as_ref());
            [Some(local.table(mode)), preset, Some(global.table(mode))]
                .into_iter()
                .flatten()
                .filter(|table| !table.is_empty())
                .map(move |table| (mode, table))
        })
    }

    /// Resolve canonical `keys` against the current buffer and locked shared definitions.
    fn match_in_tables<'a>(
        &'a self,
        groups: [Option<ModeGroup>; 2],
        keys: &[String],
        local: &'a MappingTables,
        global: &'a MappingTables,
    ) -> (Option<(usize, ModeGroup, &'a Rule)>, bool) {
        let mut exact: Option<(usize, ModeGroup, &'a Rule)> = None;
        let mut waiting = false;
        let mut nowait = false;
        for (mode, table) in self.mapping_tables(groups, local, global) {
            let shorter = exact.map_or(0, |(len, ..)| len);
            let partial = || {
                table
                    .range(keys.to_vec()..)
                    .take_while(|(lhs, _)| lhs.starts_with(keys))
                    .filter(|(lhs, _)| lhs.len() > keys.len())
            };
            if let Some((len, rule)) = (shorter + 1..=keys.len())
                .rev()
                .find_map(|len| table.get(&keys[..len]).map(|rule| (len, rule)))
            {
                exact = Some((len, mode, rule));
                nowait = !waiting
                    && len == keys.len()
                    && rule.nowait
                    && !partial().any(|(_, candidate)| candidate.order > rule.order);
            }
            waiting |= !nowait && partial().next().is_some();
        }
        (exact, waiting)
    }

    /// Resolve a held prefix before input leaves its current mapping context.
    pub(super) fn flush_input_prefix(
        &mut self,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let keys = std::mem::take(&mut self.input_maps.held);
        self.run_input_queue(
            keys,
            register,
            resolve_screen,
            InputOptions {
                source: self.input_maps.held_from,
                allow_wait: false,
            },
        )
    }

    /// Identify a pending mapping for the workspace's asynchronous timer.
    pub fn mapping_timeout_token(&self) -> Option<(u64, u64)> {
        self.input_maps
            .has_prefix()
            .then_some((self.doc.opening, self.input_maps.serial))
    }

    /// Confirm a held mapping, or replay its literal keys, at the native timeout.
    pub fn finish_mapping_timeout(
        &mut self,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.flush_input_prefix(register, resolve_screen)
    }

    /// List reachable replacements for the held prefix using the current mode and `register`.
    pub fn pending_mapping_hint(&self, register: &Register) -> Option<PendingMappingHint> {
        let held = &self.input_maps.held;
        let first = held.first()?;
        // A typed continuation can enable language mappings that a mapped RHS cannot use.
        let groups = self.input_groups(first, InputSource::Typed);
        let timeout_groups = self.input_groups(first, self.input_maps.held_from);
        self.build_mapping_hint(held, groups, timeout_groups, register)
    }

    /// Collect reachable matches for any supported pending input mode.
    fn build_mapping_hint(
        &self,
        held: &[String],
        groups: [Option<ModeGroup>; 2],
        timeout_groups: [Option<ModeGroup>; 2],
        register: &Register,
    ) -> Option<PendingMappingHint> {
        held.first()?;
        let held = canonical_input(held);
        let global = register.input_maps.0.read().unwrap();
        let global = &global.tables;
        let local = self.input_maps.local.read().unwrap();
        let (exact, _) = self.match_in_tables(timeout_groups, &held, &local, global);
        let timeout_replacement = exact
            .filter(|(len, _, _)| *len == held.len())
            .map(|(_, _, rule)| rule.replacement.clone());
        let candidates: BTreeSet<_> = self
            .mapping_tables(groups, &local, global)
            .flat_map(|(_, table)| {
                table
                    .range(held.to_vec()..)
                    .take_while(|(keys, _)| keys.starts_with(&held))
                    .filter(|(keys, _)| keys.len() > held.len())
                    .map(|(keys, _)| keys.as_slice())
            })
            .collect();
        let mut continuations = Vec::new();
        let mut terminal = None::<&[String]>;
        for keys in candidates {
            if terminal.is_some_and(|prefix| keys.starts_with(prefix)) {
                continue;
            }
            let (exact, waiting) = self.match_in_tables(groups, keys, &local, global);
            if let Some((_, _, rule)) = exact {
                continuations.push(MappingContinuation {
                    keys: display_mapping_keys(&keys[held.len()..]),
                    replacement: rule.replacement.clone(),
                });
            }
            // Sorted candidates keep every descendant of a completed sequence together.
            terminal = (!waiting).then_some(keys);
        }
        (!continuations.is_empty() || timeout_replacement.is_some()).then(|| PendingMappingHint {
            prefix: display_mapping_keys(&held),
            continuations,
            timeout_replacement,
        })
    }

    /// Prevent a shared Visual/Select mapping from restoring an area that it left.
    pub(super) fn skip_select_restore(&mut self) {
        if self.input_maps.depth > 0 {
            self.input_maps.suppress_reselect = true;
        }
    }

    /// Execute menu `replacement` with the same Select rules and `resolve_screen` as typed mappings.
    pub(super) fn run_menu_mapping(
        &mut self,
        replacement: String,
        recursive: bool,
        restore_select: bool,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let rule = Rule {
            replacement,
            source: None,
            defined_at: None,
            recursive,
            expression: false,
            restore_select,
            nowait: false,
            order: 0,
        };
        let mode = if self.g.select {
            ModeGroup::Select
        } else {
            ModeGroup::Visual
        };
        self.run_mapping(
            &rule,
            mode,
            &[],
            register,
            resolve_screen,
            InputOptions {
                source: InputSource::Replacement,
                allow_wait: true,
            },
        )
    }

    /// Run `rule` in its matched table `mode`, retaining the Select area where appropriate.
    fn run_mapping(
        &mut self,
        rule: &Rule,
        mode: ModeGroup,
        lhs: &[String],
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
        options: InputOptions,
    ) -> Action {
        if self.input_maps.depth >= RECURSION_LIMIT {
            self.message = i18n::text("vim_mapping.recursive").into();
            return Action::None;
        }
        let keys = self.mapping_replacement(rule, register);
        // Vim runs the first key literally when a recursive RHS begins with its own LHS.
        let mut literal_first = !lhs.is_empty()
            && keys.len() >= lhs.len()
            && lhs
                .iter()
                .zip(&keys)
                .all(|(left, right)| mapping_key(left) == mapping_key(right));
        let restore = (mode == ModeGroup::Select && rule.restore_select).then(|| {
            self.g.select = false;
            (self.mode, self.anchor, self.doc.cursor, self.doc.text.len())
        });
        let mut action = Action::None;
        let previous_reselect = std::mem::take(&mut self.input_maps.suppress_reselect);
        let previous_abbr = self.input_maps.suppress_abbreviation;
        let previous_macro = self.input_maps.suppress_macro_input;
        self.input_maps.suppress_abbreviation |= !rule.recursive;
        self.input_maps.suppress_macro_input |= !lhs.is_empty() && mode != ModeGroup::Language;
        self.input_maps.depth += 1;
        let mut keys = keys.into_iter().peekable();
        while let Some(key) = keys.next() {
            let literal = std::mem::take(&mut literal_first);
            action = if key == "<Cmd>" {
                let mut text = String::new();
                let mut ended = false;
                for key in keys.by_ref() {
                    if key == "Enter" {
                        ended = true;
                        break;
                    }
                    text.push_str(if key == "Tab" { "\t" } else { &key });
                }
                if ended {
                    let origin = self.menu_origin();
                    self.execute_ex_command(
                        &text,
                        self.selection(),
                        origin,
                        register,
                        resolve_screen,
                    )
                } else {
                    self.message = i18n::text("vim_mapping.command_unterminated").into();
                    Action::None
                }
            } else if rule.recursive && !literal {
                let mut pending = std::mem::take(&mut self.input_maps.held);
                pending.push(key);
                while keys.peek().is_some_and(|key| key != "<Cmd>") {
                    pending.extend(keys.next());
                }
                let source = match options.source {
                    InputSource::Typed => InputSource::Replacement,
                    source => source,
                };
                self.run_input_queue(
                    pending,
                    register,
                    resolve_screen,
                    InputOptions {
                        source,
                        allow_wait: options.allow_wait && keys.peek().is_none(),
                    },
                )
            } else {
                self.dispatch_unmapped_key(key, register, resolve_screen, options.source)
            };
            if action != Action::None {
                break;
            }
        }
        self.input_maps.depth -= 1;
        self.input_maps.suppress_abbreviation = previous_abbr;
        self.input_maps.suppress_macro_input = previous_macro;
        let suppressed = self.input_maps.suppress_reselect;
        self.input_maps.suppress_reselect =
            previous_reselect || suppressed && restore.is_none() && self.input_maps.depth > 0;
        if let Some((prior, anchor, cursor, original_len)) = restore
            && action == Action::None
        {
            if suppressed {
                self.g.select = false;
            } else if self.mode.is_visual() {
                self.g.select = true;
            } else if self.mode == Mode::Normal && self.doc.text.len() >= original_len {
                self.mode = prior;
                self.anchor = self
                    .doc
                    .text
                    .floor_char_boundary(anchor.min(self.doc.text.len()));
                self.set_cursor(cursor);
                self.g.select = true;
            }
        }
        action
    }

    /// Return executable right-side keys for `rule`, evaluating an expression against `register`.
    fn mapping_replacement(&mut self, rule: &Rule, register: &Register) -> Vec<String> {
        let leader = rule
            .source
            .as_ref()
            .map_or(LEADER_KEY, |source| source.leader.as_str());
        if rule.expression {
            match self.evaluate_expression(&rule.replacement, register) {
                Ok(value) => tokenize_with_leader(&value.display(), leader),
                Err(error) => {
                    self.message =
                        i18n::format("vim_mapping.expression_error", &[("error", &error)]);
                    Vec::new()
                }
            }
        } else {
            tokenize_with_leader(&rule.replacement, leader)
        }
    }

    /// Expand a configured abbreviation before `key` using `register` and the motion resolver.
    /// Return an action if an explicit Ctrl+] is consumed or expansion leaves the current surface.
    pub(super) fn maybe_expand_abbreviation(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Action> {
        let force = key == "Ctrl+]";
        if self.pending.is_some() || self.input_maps.suppress_abbreviation && !force {
            return None;
        }
        let group = if self.mode.is_insert() {
            ModeGroup::InsertAbbrev
        } else if self.mode == Mode::Command && self.prompt == ':' {
            ModeGroup::CommandAbbrev
        } else {
            return None;
        };
        let delimiter = matches!(
            key,
            "Escape" | "Ctrl+[" | "Enter" | "Ctrl+j" | "Ctrl+m" | "Tab" | "Ctrl+i"
        ) || key.graphemes(true).count() == 1
            && !key.chars().next().is_some_and(keyword);
        if !force && !delimiter {
            return None;
        }
        let old_mode = self.mode;
        let found = self.abbreviation(group, register);
        if let Some((range, rule)) = found {
            let keys = self.mapping_replacement(&rule, register);
            if keys.is_empty() && rule.expression && !self.message.is_empty() {
                return Some(Action::None);
            }
            if self.mode.is_insert() {
                let n = self.doc.text[range].graphemes(true).count();
                for _ in 0..n {
                    self.insert_key("Backspace");
                }
            } else {
                self.command_pos = range.start;
                self.command.replace_range(range, "");
                self.command_prompt.reset();
            }
            let previous = std::mem::replace(&mut self.input_maps.suppress_abbreviation, true);
            let mut action = Action::None;
            for key in keys {
                action = self.handle_key(&key, register, resolve_screen);
                if action != Action::None {
                    break;
                }
            }
            self.input_maps.suppress_abbreviation = previous;
            if action != Action::None || self.mode != old_mode {
                return Some(action);
            }
        }
        force.then_some(Action::None)
    }

    /// Find the longest valid abbreviation ending at the current text caret using `register`.
    fn abbreviation(&self, mode: ModeGroup, register: &Register) -> Option<(Range<usize>, Rule)> {
        let (start, at, prefix) = if self.mode.is_insert() {
            let at = self.doc.cursor;
            let start =
                self.doc.starts[self.doc.location(at).0].max(self.insert_text_start().min(at));
            (start, at, &self.doc.text[start..at])
        } else {
            let at = self.command_cursor();
            let text = &self.command[..at];
            let start = self.ex_range(text).map_or(0, |(_, rest)| at - rest.len());
            (start, at, &text[start..])
        };
        let shared = register.input_maps.0.read().unwrap();
        let local = self.input_maps.local.read().unwrap();
        let local = local.table(mode);
        let shared = shared.tables.table(mode);
        if local.is_empty() && shared.is_empty() {
            return None;
        }
        let tail = prefix.ceil_char_boundary(prefix.len().saturating_sub(KEY_LIMIT));
        prefix[tail..].char_indices().find_map(|(offset, _)| {
            let offset = tail + offset;
            let word = &prefix[offset..];
            let key = vec![word.to_owned()];
            let rule = local.get(&key).or_else(|| shared.get(&key))?;
            let before = prefix[..offset].chars().next_back();
            let boundary = match abbreviation_kind(word)? {
                Abbreviation::Full => before.is_none_or(|ch| {
                    !keyword(ch) && (word.chars().count() > 1 || matches!(ch, ' ' | '\t'))
                }),
                Abbreviation::End => {
                    before.is_none_or(|ch| keyword(ch) || matches!(ch, ' ' | '\t'))
                }
                Abbreviation::Non => before.is_none_or(|ch| matches!(ch, ' ' | '\t')),
            };
            boundary.then(|| (start + offset..at, rule.clone()))
        })
    }
}

/// Word classification for the three Vim abbreviation shapes.
enum Abbreviation {
    Full,
    End,
    Non,
}

/// Classify a nonempty abbreviation `word` with the editor's default keyword rules.
fn abbreviation_kind(word: &str) -> Option<Abbreviation> {
    if word.contains(char::is_whitespace) {
        return None;
    }
    let end = word.char_indices().next_back()?;
    if !keyword(end.1) {
        Some(Abbreviation::Non)
    } else if word.chars().all(keyword) {
        Some(Abbreviation::Full)
    } else if word[..end.0].chars().all(|ch| !keyword(ch)) {
        Some(Abbreviation::End)
    } else {
        None
    }
}

/// Whether `ch` is part of an editor keyword.
fn keyword(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Decode angle-bracket key notation and literal graphemes in a mapping `value`.
fn tokenize(value: &str) -> Vec<String> {
    tokenize_with_leader(value, LEADER_KEY)
}

/// Parse `left` for `mode`, keeping an abbreviation as a single word and resolving `leader` in keys.
fn mapping_lhs(mode: ModeGroup, left: &str, leader: &str) -> Vec<String> {
    if left.is_empty() {
        Vec::new()
    } else if mode.abbreviations() {
        vec![left.to_owned()]
    } else {
        tokenize_with_leader(left, leader)
    }
}

/// Split a sequence into graphemes and resolve Vim's key notation using `leader`.
fn tokenize_with_leader(mut value: &str, leader: &str) -> Vec<String> {
    let mut keys = Vec::new();
    while !value.is_empty() && keys.len() < KEY_LIMIT {
        if let Some(rest) = value.strip_prefix('<')
            && let Some((name, tail)) = rest.split_once('>')
        {
            if name.eq_ignore_ascii_case("nop") {
                value = tail;
                continue;
            }
            if name.eq_ignore_ascii_case("leader") {
                keys.extend(
                    leader
                        .graphemes(true)
                        .take(KEY_LIMIT - keys.len())
                        .map(super::register::text_key),
                );
                value = tail;
                continue;
            }
            if let Some(key) = named_key(name, leader) {
                keys.push(key);
                value = tail;
                continue;
            }
        }
        let grapheme = value.graphemes(true).next().expect("nonempty mapping");
        keys.push(grapheme.to_owned());
        value = &value[grapheme.len()..];
    }
    keys
}

/// Match synonymous control keys without changing the native command used on a miss.
fn mapping_key(key: &str) -> &str {
    match key {
        "Ctrl+i" => "Tab",
        "Ctrl+m" => "Enter",
        "Ctrl+[" => "Escape",
        "Ctrl+^" => "Ctrl+6",
        "Ctrl+ " => "Ctrl+space",
        _ => key,
    }
}

/// Normalize Vim's synonymous keys for matching without rewriting the held input.
fn canonical_input(keys: &[String]) -> Cow<'_, [String]> {
    if keys.iter().any(|key| mapping_key(key) != key) {
        Cow::Owned(keys.iter().map(|key| mapping_key(key).to_owned()).collect())
    } else {
        Cow::Borrowed(keys)
    }
}

/// Render a sequence compactly, retaining boundaries around symbolic keys.
fn display_mapping_keys(keys: &[String]) -> String {
    keys.iter().map(|key| display_key(key)).collect()
}

/// Render a stored symbolic key in notation that the Ex mapping parser accepts.
fn display_key(key: &str) -> String {
    match key {
        " " => "<Space>".into(),
        "<" => "<lt>".into(),
        "Escape" => "<Esc>".into(),
        "Enter" => "<CR>".into(),
        key if key.graphemes(true).count() == 1 => key.into(),
        key => format!(
            "<{}>",
            key.replace("Ctrl+", "C-")
                .replace("Alt+", "A-")
                .replace("Shift+", "S-")
        ),
    }
}

/// Convert a Vim key `name` to a symbolic key, using `leader` in modified chords.
fn named_key(name: &str, leader: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if let Some(number) = lower.strip_prefix("char-") {
        return unicode_key(number).map(|key| key.to_string());
    }
    if let Some(key) = modified_character(name, leader) {
        return Some(key);
    }
    let key = match lower.as_str() {
        "cmd" => "<Cmd>",
        "esc" | "escape" => "Escape",
        "cr" | "enter" | "return" => "Enter",
        "bs" | "backspace" => "Backspace",
        "del" | "delete" => "Delete",
        "tab" => "Tab",
        "space" => " ",
        "lt" => "<",
        "bar" => "|",
        "bslash" => "\\",
        "localleader" => "\\",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "home" => "Home",
        "end" => "End",
        "insert" => "Insert",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        _ => {
            if let Some(ch) = lower
                .strip_prefix("c-")
                .filter(|key| key.chars().count() == 1)
            {
                return Some(match ch {
                    "i" => "Tab".into(),
                    "m" => "Enter".into(),
                    "[" => "Escape".into(),
                    "^" => "Ctrl+6".into(),
                    _ => format!("Ctrl+{ch}"),
                });
            }
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
                && (1..=24).contains(&n)
            {
                return Some(format!("F{n}"));
            }
            return None;
        }
    };
    Some(key.into())
}

/// Canonicalize a chord in `name`, using `leader` if it contains the leader key.
fn modified_character(mut name: &str, leader: &str) -> Option<String> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    while let Some((modifier, rest)) = name.split_once('-') {
        match modifier.to_ascii_lowercase().as_str() {
            "c" => ctrl = true,
            "a" | "m" => alt = true,
            "s" => shift = true,
            _ => break,
        }
        name = rest;
    }
    if !ctrl && !alt && !shift {
        return None;
    }
    let prefix = match (ctrl, alt, shift) {
        (true, true, true) => "Ctrl+Alt+Shift+",
        (true, true, false) => "Ctrl+Alt+",
        (true, false, true) => "Ctrl+Shift+",
        (true, false, false) => "Ctrl+",
        (false, true, true) => "Alt+Shift+",
        (false, true, false) => "Alt+",
        (false, false, true) => "Shift+",
        _ => return None,
    };
    if name.eq_ignore_ascii_case("space") && ctrl && !alt && !shift {
        return Some("Ctrl+space".into());
    }
    let mut character = if let Some(number) = name.to_ascii_lowercase().strip_prefix("char-") {
        unicode_key(number)?
    } else if name.eq_ignore_ascii_case("space") {
        ' '
    } else if name.eq_ignore_ascii_case("leader") {
        let only = leader.chars().next()?;
        if only.len_utf8() != leader.len() {
            return None;
        }
        only
    } else {
        let mut graphemes = name.chars();
        let key = graphemes.next()?;
        if graphemes.next().is_some() {
            let key = named_key(name, leader)?;
            return Some(format!("{prefix}{key}"));
        }
        key
    };
    if shift {
        character = character.to_ascii_uppercase();
    }
    Some(match (ctrl, alt, shift) {
        (true, false, false) => {
            mapping_key(&format!("Ctrl+{}", character.to_ascii_lowercase())).to_owned()
        }
        (false, false, true) => character.to_string(),
        (false, true, true) => format!("Alt+{character}"),
        (true, true, true) => format!("Ctrl+Alt+{character}"),
        _ => format!("{prefix}{character}"),
    })
}

/// Decode the decimal, hexadecimal or leading-zero octal value in `number`.
fn unicode_key(number: &str) -> Option<char> {
    let (radix, digits) = if let Some(value) = number.strip_prefix("0x") {
        (16, value)
    } else if number.starts_with('0') && number.len() > 1 {
        (8, number)
    } else {
        (10, number)
    };
    char::from_u32(u32::from_str_radix(digits, radix).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Enter an Ex `command` without its colon and return the submitted action.
    fn ex(vim: &mut Vim, register: &mut Register, command: &str) -> Action {
        vim.key(":", register);
        vim.paste_text(command, register);
        vim.key("Enter", register)
    }

    /// Keep a buffer-local mapping shared across views, including definitions made after split.
    #[test]
    fn buffer_local_mappings_follow_shared_views_and_detach_with_the_buffer() {
        let mut original = Vim::new(Document::new("one two"));
        let mut view = Vim::new(original.doc.shared_view());
        view.share_buffer_options(&original);
        let mut original_register = Register::default();
        let mut view_register = Register::default();
        let other = Vim::new(original.doc.fork());

        ex(
            &mut original,
            &mut original_register,
            "nnoremap <buffer> <F7> w",
        );
        view.key("F7", &mut view_register);
        assert_eq!(view.doc.cursor, 4);
        assert!(!other.accepts_input_mapping("F7", &view_register));

        view.doc.detach_buffer();
        view.detach_buffer_options();
        ex(
            &mut original,
            &mut original_register,
            "nunmap <buffer> <F7>",
        );
        assert!(!original.accepts_input_mapping("F7", &original_register));
        assert!(view.accepts_input_mapping("F7", &view_register));
    }

    /// Show only applicable mode and scope, with the exact fallback when the prefix times out.
    #[test]
    fn pending_mapping_hint_uses_the_same_mode_and_priority_as_input() {
        let mut vim = Vim::new(Document::new("one two"));
        let mut register = Register::default();
        for command in [
            "nnoremap za :echo 'global'<CR>",
            "nnoremap <buffer> za :echo 'local'<CR>",
            "nnoremap zab :write<CR>",
            "nnoremap z<F3> :buffers<CR>",
            "inoremap zi <Esc>",
        ] {
            assert_eq!(ex(&mut vim, &mut register, command), Action::None);
        }

        vim.key("z", &mut register);
        let hint = vim.pending_mapping_hint(&register).unwrap();
        assert_eq!(hint.prefix, "z");
        assert!(hint.timeout_replacement.is_none());
        let rows: Vec<_> = hint
            .continuations
            .iter()
            .map(|row| (row.keys.as_str(), row.replacement.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("<F3>", ":buffers<CR>"),
                ("a", ":echo 'local'<CR>"),
                ("ab", ":write<CR>"),
            ]
        );

        vim.key("a", &mut register);
        let hint = vim.pending_mapping_hint(&register).unwrap();
        assert_eq!(hint.prefix, "za");
        assert_eq!(
            hint.timeout_replacement.as_deref(),
            Some(":echo 'local'<CR>")
        );
        assert_eq!(hint.continuations.len(), 1);
        assert_eq!(hint.continuations[0].keys, "b");
        assert_eq!(hint.continuations[0].replacement, ":write<CR>");
        vim.finish_mapping_timeout(&mut register, &mut |_, _, _, _| None);
        assert_eq!(vim.message, "local");

        vim.key("i", &mut register);
        vim.commit_text("z", &mut register);
        let hint = vim.pending_mapping_hint(&register).unwrap();
        assert_eq!(hint.continuations.len(), 1);
        assert_eq!(hint.continuations[0].keys, "i");
        assert_eq!(hint.continuations[0].replacement, "<Esc>");
    }

    /// Omit a longer sequence that an earlier nowait rule will consume before it can be typed.
    #[test]
    fn pending_mapping_hint_hides_unreachable_nowait_descendants() {
        let mut vim = Vim::new(Document::new("one two"));
        let mut register = Register::default();
        for command in [
            "nnoremap <Leader>ax gg",
            "nnoremap <nowait> <buffer> <Leader>a w",
            "nnoremap <expr> <Leader>b 'w'",
        ] {
            assert_eq!(ex(&mut vim, &mut register, command), Action::None);
        }
        vim.key(LEADER_KEY, &mut register);
        let hint = vim.pending_mapping_hint(&register).unwrap();
        assert_eq!(hint.prefix, "<Space>");
        let rows: Vec<_> = hint
            .continuations
            .iter()
            .map(|row| (row.keys.as_str(), row.replacement.as_str()))
            .collect();
        assert_eq!(rows, [("a", "w"), ("b", "'w'")]);
        vim.key("a", &mut register);
        assert!(vim.pending_mapping_hint(&register).is_none());
        assert_eq!(vim.doc.cursor, 4);
    }

    /// A typed key can use the language table after a recursive mapping leaves a prefix pending.
    #[test]
    fn pending_mapping_hint_follows_typed_continuations_after_recursive_mapping() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        for command in [
            "imap x a",
            "inoremap a fallback",
            "inoremap ab body",
            "lmap a language",
            "lnoremap ac continuation",
            "set iminsert=1",
        ] {
            assert_eq!(ex(&mut vim, &mut register, command), Action::None);
        }
        vim.key("i", &mut register);
        vim.key("x", &mut register);
        let hint = vim.pending_mapping_hint(&register).unwrap();
        assert_eq!(hint.prefix, "a");
        assert_eq!(hint.timeout_replacement.as_deref(), Some("fallback"));
        let rows: Vec<_> = hint
            .continuations
            .iter()
            .map(|row| (row.keys.as_str(), row.replacement.as_str()))
            .collect();
        assert_eq!(rows, [("b", "body"), ("c", "continuation")]);
        vim.key("c", &mut register);
        assert_eq!(vim.doc.text, "continuation");
    }

    /// Normal, Insert, command-line/search, and operator-pending share one dispatcher.
    #[test]
    fn general_mappings_follow_the_active_input_mode() {
        let mut vim = Vim::new(Document::new("one two three"));
        let mut register = Register::default();
        for command in [
            "nnoremap <F6> w",
            "onoremap <F6> iw",
            "onoremap w e",
            "inoremap jj <Esc>",
            "inoremap <expr> <F6> 'λ'",
            "cnoremap <C-S-Left> three",
            "cnoremap <F6> echo 'mapped'<CR>",
        ] {
            assert_eq!(ex(&mut vim, &mut register, command), Action::None);
        }
        vim.key("F6", &mut register);
        assert_eq!(vim.doc.cursor, 4);
        vim.text("diw", &mut register);
        assert_eq!(vim.doc.text, "one  three");
        vim.text("0d", &mut register);
        assert!(vim.accepts_input_mapping("F6", &register));
        vim.key("F6", &mut register);
        assert_eq!(vim.doc.text, "  three");
        vim.key("i", &mut register);
        vim.key("F6", &mut register);
        vim.commit_text("j", &mut register);
        assert!(vim.mapping_timeout_token().is_some());
        vim.paste_text("jj", &mut register);
        assert_eq!(vim.doc.text, "λjjj  three");
        assert!(vim.mapping_timeout_token().is_none());
        vim.commit_text("jj", &mut register);
        assert_eq!(vim.mode, Mode::Normal);
        vim.key("/", &mut register);
        vim.key("Ctrl+Shift+Left", &mut register);
        assert_eq!(vim.command, "three");
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.cursor, "λjjj  ".len());
        vim.key(":", &mut register);
        vim.key("F6", &mut register);
        assert_eq!(vim.message, "mapped");
        assert_eq!(vim.mode, Mode::Normal);
    }

    /// List canonical keys and preserve both workspace and buffer scopes during edits.
    #[test]
    fn map_commands_list_and_remove_only_the_requested_scope() {
        let mut vim = Vim::new(Document::new("one two three"));
        let mut other = Vim::new(Document::new("one two three"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "noremap <F7> w");
        ex(&mut vim, &mut register, "nnoremap <buffer> <F7> l");
        ex(&mut vim, &mut register, "nmap <F7>");
        assert!(vim.message.contains("<F7>"));
        assert!(vim.message.contains("@l"));
        assert!(vim.message.contains("  w"));
        ex(&mut vim, &mut register, "nmap <buffer> <F7>");
        assert!(!vim.message.contains("  w"));
        vim.key("F7", &mut register);
        other.key("F7", &mut register);
        assert_eq!((vim.doc.cursor, other.doc.cursor), (1, 4));
        ex(&mut vim, &mut register, "nunmap <buffer> <F7>");
        vim.key("F7", &mut register);
        assert_eq!(vim.doc.cursor, 4);
        ex(&mut vim, &mut register, "nmapclear");
        assert!(!vim.accepts_input_mapping("F7", &register));
        vim.key("y", &mut register);
        assert!(vim.accepts_input_mapping("F7", &register));
        vim.key("Escape", &mut register);
        ex(&mut vim, &mut register, "noremap! <F8> z");
        vim.key("i", &mut register);
        vim.key("F8", &mut register);
        assert_eq!(vim.doc.text, "one ztwo three");
        vim.key("Escape", &mut register);
        vim.key(":", &mut register);
        vim.key("F8", &mut register);
        assert_eq!(vim.command, "z");
        vim.key("Escape", &mut register);
        ex(&mut vim, &mut register, "mapclear!");
        vim.key(":", &mut register);
        assert!(!vim.accepts_input_mapping("F8", &register));
    }

    /// Resolve new mappings after literal mode changes, while honoring recursive and nowait rules.
    #[test]
    fn map_resolution_rechecks_the_mode_and_honors_scope_precedence() {
        let mut vim = Vim::new(Document::new("abc def"));
        let mut register = Register::default();
        for command in [
            "nnoremap <F2> w",
            "nmap <F3> <F2>",
            "nnoremap <F4> <F2>",
            "nnoremap ia <Nop>",
            "inoremap x Y",
            "nnoremap aa 0",
            "nnoremap <buffer> <nowait> a l",
            "nnoremap <buffer> zx 0",
            "nnoremap <nowait> z w",
            "nmap qb qbcd<Esc>",
        ] {
            ex(&mut vim, &mut register, command);
        }
        vim.key("F4", &mut register);
        assert_eq!(vim.doc.cursor, 0);
        vim.key("F3", &mut register);
        assert_eq!(vim.doc.cursor, 4);
        vim.key("a", &mut register);
        assert_eq!(vim.doc.cursor, 5);
        assert!(vim.mapping_timeout_token().is_none());
        vim.key("z", &mut register);
        assert!(vim.mapping_timeout_token().is_some());
        vim.key("x", &mut register);
        assert_eq!(vim.doc.cursor, 0);
        vim.text("ix", &mut register);
        assert_eq!(vim.doc.text, "Yabc def");
        vim.key("Escape", &mut register);
        vim.text("qb", &mut register);
        assert!(
            register.is_recording_macro(),
            "self-prefix executes the native q first"
        );
    }

    /// Hand a partial Leader map back to the shell only when no shorter rule would run.
    #[test]
    fn unmatched_leader_prefix_can_return_to_the_workspace() {
        let mut vim = Vim::new(Document::new("abc def"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "nnoremap <Leader>x w");
        assert!(vim.accepts_input_mapping(LEADER_KEY, &register));
        vim.key(LEADER_KEY, &mut register);
        assert!(!vim.accepts_input_mapping("f", &register));
        assert_eq!(
            vim.take_unmapped_shell_prefix(&register),
            Some(vec![LEADER_KEY.into()])
        );
        assert!(vim.mapping_timeout_token().is_none());
        ex(&mut vim, &mut register, "nnoremap <Space> l");
        vim.key(LEADER_KEY, &mut register);
        assert!(vim.take_unmapped_shell_prefix(&register).is_none());
        vim.finish_mapping_timeout(&mut register, &mut |_, _, _, _| None);
        assert_eq!(vim.doc.cursor, 1);
    }

    /// A macro stores the typed map keys, while dot preserves the edit that already ran.
    #[test]
    fn mapped_macro_keys_remap_on_playback_but_dot_repeats_the_edit() {
        let mut vim = Vim::new(Document::new("abc def\ntwo\nthree"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "nnoremap W x");
        vim.text("qaWq", &mut register);
        assert_eq!(vim.doc.text, "bc def\ntwo\nthree");
        assert_eq!(register.macro_keys('a')[0].register_text(), "W");
        ex(&mut vim, &mut register, "nnoremap W dd");
        vim.key(".", &mut register);
        assert_eq!(vim.doc.text, "c def\ntwo\nthree");
        vim.text("@a", &mut register);
        assert_eq!(vim.doc.text, "two\nthree");
    }

    /// :normal uses the same mappings as keyboard input and :normal! feeds original keys.
    #[test]
    fn ex_normal_resolves_mappings_but_its_bang_variant_is_literal() {
        let mut vim = Vim::new(Document::new("abc def\ntwo"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "nnoremap W x");
        ex(&mut vim, &mut register, "normal W");
        assert_eq!(vim.doc.text, "bc def\ntwo");
        ex(&mut vim, &mut register, "normal! W");
        assert_eq!(vim.doc.text, "bc def\ntwo");
        assert_eq!(vim.doc.cursor, 3);
        ex(&mut vim, &mut register, "1,2normal W");
        assert_eq!(vim.doc.text, "c def\nwo");
    }

    /// Cyclic recursive maps are bounded even when a longer match is waiting on a timeout.
    #[test]
    fn recursive_map_cannot_rearm_a_timeout_indefinitely() {
        let mut vim = Vim::new(Document::new("abc def"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "nmap Q W");
        ex(&mut vim, &mut register, "nmap W Q");
        ex(&mut vim, &mut register, "nnoremap QQ x");
        vim.key("Q", &mut register);
        assert!(vim.mapping_timeout_token().is_some());
        vim.finish_mapping_timeout(&mut register, &mut |_, _, _, _| None);
        assert!(vim.mapping_timeout_token().is_none());
        assert_eq!(vim.message, i18n::text("vim_mapping.recursive"));
        assert_eq!(vim.doc.text, "abc def");
    }

    /// Language maps start disabled and match longer prefixes, literal arguments and toggle state.
    #[test]
    fn language_maps_match_sequences_and_toggle_by_mode() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "lmap a ä");
        ex(&mut vim, &mut register, "lnoremap ab 中文");
        vim.text("iab", &mut register);
        assert_eq!(vim.doc.text, "ab");
        vim.key("Ctrl+6", &mut register);
        vim.commit_text("ab", &mut register);
        assert_eq!(vim.doc.text, "ab中文");
        vim.commit_text("a", &mut register);
        assert!(vim.mapping_timeout_token().is_some());
        vim.finish_mapping_timeout(&mut register, &mut |_, _, _, _| None);
        assert_eq!(vim.doc.text, "ab中文ä");
        vim.paste_text("ab", &mut register);
        assert_eq!(vim.doc.text, "ab中文äab");
        vim.key("Escape", &mut register);
        vim.text("0fa", &mut register);
        vim.finish_mapping_timeout(&mut register, &mut |_, _, _, _| None);
        assert_eq!(vim.doc.cursor, "ab中文".len());
        vim.key(":", &mut register);
        vim.commit_text("a", &mut register);
        assert_eq!(vim.command, "a");
        vim.key("Escape", &mut register);
        vim.key("/", &mut register);
        vim.commit_text("ab", &mut register);
        assert_eq!(vim.command, "中文");
    }

    /// Load a project keymap while retaining separate mappings and earlier buffers' defaults.
    #[test]
    fn keymap_option_loads_runtime_rules_and_buffer_defaults() {
        let project = tempfile::tempdir().unwrap();
        let keymaps = project.path().join("keymap");
        std::fs::create_dir(&keymaps).unwrap();
        std::fs::write(
            keymaps.join("scope-test.vim"),
            "loadkeymap\ng п\nh р\nb и\nd в\nt е\nn т\nq й\nr к\n",
        )
        .unwrap();
        let mut vim = Vim::new(Document::new(""));
        vim.set_completion_directory(project.path());
        let mut other = Vim::new(Document::new(""));
        let mut register = Register::default();
        other.key("i", &mut register);
        other.key("Escape", &mut register);
        ex(&mut vim, &mut register, "lmap q !");
        ex(&mut vim, &mut register, "lmap <buffer> 0 №");
        ex(&mut vim, &mut register, "set imsearch=0");
        ex(&mut vim, &mut register, "set keymap=scope-test");
        ex(&mut vim, &mut register, "set keymap?");
        assert_eq!(vim.message, "keymap=scope-test");
        ex(&mut vim, &mut register, "set imsearch?");
        assert_eq!(vim.message, "imsearch=1");
        assert!(
            vim.command_candidates("set keymap=scope-te", &register)
                .unwrap()
                .1
                .iter()
                .any(|keymap| keymap == "scope-test")
        );
        vim.text("ighbdtnq", &mut register);
        assert_eq!(vim.doc.text, "приветй");
        vim.key("Ctrl+^", &mut register);
        vim.commit_text("q", &mut register);
        assert_eq!(vim.doc.text, "приветйq");
        vim.key("Escape", &mut register);
        ex(
            &mut vim,
            &mut register,
            "set keymap=nanobug-test-unavailable",
        );
        assert!(vim.message.contains("nanobug-test-unavailable"));
        ex(&mut vim, &mut register, "set keymap?");
        assert_eq!(vim.message, "keymap=scope-test");
        other.text("ighbdtn", &mut register);
        assert_eq!(other.doc.text, "ghbdtn");
        let mut fresh = Vim::new(Document::new(""));
        fresh.text("ighbdtn", &mut register);
        assert_eq!(fresh.doc.text, "привет");
        ex(&mut vim, &mut register, "setlocal keymap=");
        ex(&mut vim, &mut register, "set imsearch?");
        assert_eq!(vim.message, "imsearch=-1");
        vim.key("A", &mut register);
        vim.key("Ctrl+^", &mut register);
        vim.commit_text("q0", &mut register);
        assert_eq!(vim.doc.text, "приветйq!№");
        fresh.commit_text("r", &mut register);
        assert_eq!(fresh.doc.text, "приветк");
    }

    /// Prefer UTF-8 runtime files and parse Vim notation without executing their headers.
    #[test]
    fn keymap_option_reads_project_files_and_dead_keys() {
        let directory = tempfile::tempdir().unwrap();
        let keymaps = directory.path().join("keymap");
        std::fs::create_dir(&keymaps).unwrap();
        std::fs::write(keymaps.join("demo.vim"), "loadkeymap\na\tbad\n").unwrap();
        std::fs::write(keymaps.join("demo_utf-8.vim"),
            "let b:keymap_name = \"demo\"\nsilent quit\nloadk\na\t<char-0x00e1>\n'ab\t<char-0141><char-98><Space>\n\\\"\t中\tthis is a comment\n"
        ).unwrap();
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.set_completion_directory(directory.path());
        ex(&mut vim, &mut register, "setlocal kmp=demo");
        vim.text("ia'ab\"", &mut register);
        assert_eq!(vim.doc.text, "áab 中");
        assert_eq!(vim.active_keymap_label(), Some("demo"));
        vim.key("Escape", &mut register);
        ex(&mut vim, &mut register, "lunmap <buffer> a");
        vim.text("Aa", &mut register);
        assert_eq!(vim.doc.text, "áab 中a");
    }

    /// Decode escaped punctuation and modifier chords from a user-supplied layout.
    #[test]
    fn keymap_option_decodes_punctuation_and_modifier_chords() {
        let project = tempfile::tempdir().unwrap();
        let keymaps = project.path().join("keymap");
        std::fs::create_dir(&keymaps).unwrap();
        std::fs::write(
            keymaps.join("symbols-test.vim"),
            "loadkeymap\n\\| /\n<A-1> №\n<C-A-E> У<char-0x0301>\n",
        )
        .unwrap();
        let mut vim = Vim::new(Document::new(""));
        vim.set_completion_directory(project.path());
        let mut register = Register::default();
        ex(&mut vim, &mut register, "set keymap=symbols-test");
        vim.text("i|", &mut register);
        assert_eq!(vim.doc.text, "/");
        assert!(vim.accepts_language_chord("Alt+1", &register));
        vim.key("Alt+1", &mut register);
        assert!(vim.accepts_language_chord("Ctrl+Alt+E", &register));
        vim.key("Ctrl+Alt+E", &mut register);
        assert_eq!(vim.doc.text, "/№У\u{0301}");
    }

    /// Forced expansion adds no delimiter, and an abbreviation may contain keys and expressions.
    #[test]
    fn abbreviations_expand_with_delimiters_and_explicit_trigger() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "iabbrev teh the");
        ex(&mut vim, &mut register, "iabbrev fn fn()<Left>");
        ex(&mut vim, &mut register, "iabbrev <expr> num 2 + 3");
        ex(&mut vim, &mut register, "cabbrev xitx wq");
        vim.key("i", &mut register);
        vim.commit_text("teh ", &mut register);
        vim.commit_text("num ", &mut register);
        vim.commit_text("fn", &mut register);
        vim.key("Ctrl+]", &mut register);
        assert_eq!(vim.doc.text, "the 5 fn()");
        assert_eq!(vim.doc.cursor, "the 5 fn(".len());
        vim.key("Escape", &mut register);
        assert_eq!(ex(&mut vim, &mut register, "xitx"), Action::SaveClose);
        ex(&mut vim, &mut register, "iunabbrev teh");
        vim.key("A", &mut register);
        vim.commit_text(" teh ", &mut register);
        assert!(vim.doc.text.ends_with(" teh "));
    }

    /// Global definitions reach another buffer, while a local abbreviation stays with its owner.
    #[test]
    fn local_rules_shadow_but_do_not_mutate_shared_definitions() {
        let mut local = Vim::new(Document::new(""));
        let mut register = Register::default();
        ex(&mut local, &mut register, "iabbrev xx global");
        ex(&mut local, &mut register, "iabbrev <buffer> xx local");
        local.key("i", &mut register);
        local.commit_text("xx ", &mut register);
        assert_eq!(local.doc.text, "local ");
        let mut other = Vim::new(Document::new(""));
        other.key("i", &mut register);
        other.commit_text("xx ", &mut register);
        assert_eq!(other.doc.text, "global ");
    }

    /// Compare automatic Select restoration with the mapped gV command.
    #[test]
    fn gv_suppresses_reselection_after_shared_visual_mapping() {
        let mut vim = Vim::new(Document::new("abcdef"));
        let mut register = Register::default();
        ex(&mut vim, &mut register, "vnoremap <F2> <Esc>");
        ex(&mut vim, &mut register, "vnoremap <F3> <Esc>gV");
        ex(&mut vim, &mut register, "vnoremap <F4> U");
        ex(&mut vim, &mut register, "vnoremap <F8> d");
        vim.text("gh", &mut register);
        vim.key("Ctrl+o", &mut register);
        vim.key("l", &mut register);
        assert!(vim.g.select);
        vim.key("F2", &mut register);
        assert!(vim.g.select);
        assert_eq!(vim.selection(), Some(0..2));
        vim.key("F4", &mut register);
        assert!(vim.g.select);
        assert_eq!(vim.doc.text, "ABcdef");
        vim.key("F3", &mut register);
        assert!(!vim.g.select);
        assert_eq!(vim.mode, Mode::Normal);
        assert_eq!(vim.selection(), None);
        assert_eq!(vim.doc.cursor, 1);
        vim.text("0gh", &mut register);
        vim.key("F8", &mut register);
        assert_eq!(vim.mode, Mode::Normal);
        assert_eq!(vim.doc.text, "Bcdef");
    }
}
