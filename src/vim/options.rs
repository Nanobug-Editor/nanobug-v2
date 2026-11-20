//! Vim defaults, window-local display and buffer-local editing options.
mod preferences;

use super::{ExOrigin, InputMappings, Register, Vim};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

/// A generous native-format bound on numeric indentation settings.
pub(super) const MAX_COLUMNS: usize = 9999;

/// Default local values and workspace-wide settings saved by :vimconfig.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct VimOptions {
    /// String captured by a user mapping each time it expands `<Leader>`.
    pub mapleader: String,
    /// Wait for a longer mapping to disambiguate a prefix.
    pub timeout: bool,
    /// Wait in milliseconds for more keys after a mapping prefix.
    pub timeoutlen: u32,
    /// Fold case when compiling a search pattern unless explicitly overridden.
    pub ignorecase: bool,
    /// Keep patterns with uppercase characters case-sensitive even with ignorecase.
    pub smartcase: bool,
    /// Highlight matches of the most recent search pattern.
    pub hlsearch: bool,
    /// Preview matches of the search prompt while typing.
    pub incsearch: bool,
    /// Rows to retain on either side of the caret when possible.
    pub scrolloff: usize,
    /// Default search directories used by :find and its variants.
    pub path: String,
    /// Automatically reread clean buffers when checking their backing files.
    pub autoread: bool,
    /// Allow an explicit write destination to replace an existing file.
    pub writeany: bool,
    /// Interpret :cd without an operand as a request for the home directory.
    pub cdhome: bool,
    /// Search path for relative :cd, :tcd and :lcd operands.
    pub cdpath: String,
    /// Override the portable directory used for numbered :mkview files.
    pub viewdir: String,
    /// Ordered roots for explicitly sourcing local Vim runtime scripts.
    pub runtimepath: String,
    /// Diagnostic detail emitted by Ex commands and the local script interpreter.
    pub verbose: u32,
    /// Append Ex messages and diagnostics to this file when set.
    pub verbosefile: String,
    /// Run automatic Vim filetype detection for buffers with no assigned type.
    pub filetype_detection: bool,
    /// Default values for the next buffer created after applying these settings.
    pub(super) buffer: BufferOptions,
    /// Default values for each newly created window.
    pub(super) window: WindowOptions,
}

impl Default for VimOptions {
    fn default() -> Self {
        Self {
            mapleader: super::LEADER_KEY.into(),
            timeout: true,
            timeoutlen: 1000,
            ignorecase: false,
            smartcase: false,
            hlsearch: false,
            incsearch: true,
            scrolloff: 3,
            path: if cfg!(windows) {
                ".,,"
            } else {
                ".,/usr/include,,"
            }
            .into(),
            autoread: true,
            writeany: false,
            cdhome: !cfg!(windows),
            cdpath: std::env::var_os("CDPATH").map_or_else(
                || ",,".into(),
                |paths| {
                    std::env::split_paths(&paths).fold(String::from(","), |mut list, path| {
                        if list.len() > 1 {
                            list.push(',');
                        }
                        list.push_str(
                            &path
                                .to_string_lossy()
                                .replace(',', "\\,")
                                .replace(' ', "\\ "),
                        );
                        list
                    })
                },
            ),
            viewdir: String::new(),
            runtimepath: String::new(),
            verbose: 0,
            verbosefile: String::new(),
            filetype_detection: true,
            buffer: BufferOptions::default(),
            window: WindowOptions::default(),
        }
    }
}

impl VimOptions {
    /// Check ranges before an externally edited settings file becomes active.
    pub fn validate(&self) -> bool {
        valid_leader(&self.mapleader)
            && (1..=MAX_COLUMNS).contains(&self.buffer.tabstop)
            && self.buffer.shiftwidth <= MAX_COLUMNS
            && self.buffer.textwidth <= MAX_COLUMNS
    }

    /// Export nondefault global settings using sourceable :set commands.
    pub fn ex_export_lines(&self) -> Vec<String> {
        let defaults = Self::default();
        let mut current = self.clone();
        let mut current_window = current.window;
        let mut current_buffer = current.buffer.clone();
        let mut default_options = defaults.clone();
        let mut default_window = defaults.window;
        let mut default_buffer = defaults.buffer.clone();
        let lines = NAMES.iter().filter_map(|&(name, _)| {
            let current = set_option(
                name,
                Setting::Query,
                Scope::Global,
                false,
                &mut current,
                &mut current_window,
                &mut current_buffer,
            )
            .ok()
            .flatten()?;
            let default = set_option(
                name,
                Setting::Query,
                Scope::Global,
                false,
                &mut default_options,
                &mut default_window,
                &mut default_buffer,
            )
            .ok()
            .flatten()?;
            (current != default).then(|| format!("set {}", ex_escape_option(&current)))
        });
        lines
            .chain((!self.filetype_detection).then(|| "filetype off".into()))
            .collect()
    }
}

/// The editing options visible in all views of one buffer.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct BufferOptions {
    /// Options explicitly assigned locally stay independent of settings-page edits.
    #[serde(skip)]
    local_defaults: u16,
    /// Visual spacing of a literal tab in this buffer.
    tabstop: usize,
    /// Indentation width. Zero follows tabstop.
    shiftwidth: usize,
    /// Preferred column count for Ex alignment. Zero uses the command fallback.
    textwidth: usize,
    /// Insert spaces for typed tabs and generated indentation.
    expandtab: bool,
    /// Carry the current line's indentation into new lines.
    autoindent: bool,
    /// Override the global 'path' for this shared buffer.
    #[serde(skip)]
    path: Option<Arc<str>>,
    /// Override the global 'autoread' for this shared buffer.
    #[serde(skip)]
    autoread: Option<bool>,
    /// Protect writes to this buffer's backing file without blocking edits.
    #[serde(skip)]
    readonly: bool,
    /// Permit a local crash recovery file for this shared buffer.
    swapfile: bool,
    /// Filetype supplied explicitly or by :filetype detect for this buffer.
    #[serde(skip)]
    filetype: super::ex_languages::BufferFiletype,
    /// Locations of explicit source assignments, shared with other views of this buffer.
    #[serde(skip)]
    origins: BTreeMap<&'static str, ExOrigin>,
}

impl Default for BufferOptions {
    fn default() -> Self {
        Self {
            tabstop: 4,
            local_defaults: 0,
            shiftwidth: 4,
            textwidth: 0,
            expandtab: false,
            autoindent: true,
            path: None,
            autoread: None,
            readonly: false,
            swapfile: true,
            filetype: Default::default(),
            origins: BTreeMap::new(),
        }
    }
}

/// Display options retained independently by each editor window.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct WindowOptions {
    /// Explicit per-view overrides are kept when the workspace edits its defaults.
    #[serde(skip)]
    local_defaults: u16,
    /// Display absolute line numbers, including on the current line in relative mode.
    pub number: bool,
    /// Display line distances from the current line.
    pub relativenumber: bool,
    /// Override the normal Markdown-dependent soft-wrap default.
    pub wrap: Option<bool>,
    /// Scroll together with other bound views on the current Tabpage.
    pub scrollbind: bool,
    /// Override the shared scroll margin for this window.
    #[serde(skip)]
    pub scrolloff: Option<usize>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            number: true,
            local_defaults: 0,
            relativenumber: true,
            wrap: None,
            scrollbind: false,
            scrolloff: None,
        }
    }
}

impl Register {
    /// Check whether this pane already uses the workspace's shared configuration.
    pub fn has_input_mappings(&self, mappings: &InputMappings) -> bool {
        self.window_options.is_some() && self.input_maps.same_instance(mappings)
    }

    /// Give an entering buffer the current workspace editing defaults.
    pub fn initialize_buffer_options(&self, vim: &mut Vim) {
        if let Some(defaults) = self.window_options {
            vim.set_input_options(&self.input_maps);
            vim.view_options.get_or_insert(defaults);
        }
    }

    /// Return the default values for a buffer's first view in this window.
    pub fn window_options(&self) -> WindowOptions {
        self.window_options
            .unwrap_or_else(|| self.input_maps.with_options(|options| options.window))
    }

    /// Apply copied display options to a newly split window.
    pub fn set_window_options(&mut self, options: WindowOptions) {
        self.window_options = Some(options);
    }
}

/// Whether to use local values, defaults, or both in the current :set operation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Both,
    Local,
    Global,
}

/// An operation following an option's canonical name or alias.
#[derive(Clone, Copy)]
enum Setting<'a> {
    Query,
    Boolean(bool),
    Invert,
    Assign(&'a str),
    Append(&'a str),
    Prepend(&'a str),
    Remove(&'a str),
    Reset,
    Inherit,
}

/// The options implemented by :set and exposed in command-line completion.
const NAMES: &[(&str, &str)] = &[
    ("number", "nu"),
    ("relativenumber", "rnu"),
    ("wrap", ""),
    ("scrollbind", "scb"),
    ("tabstop", "ts"),
    ("shiftwidth", "sw"),
    ("textwidth", "tw"),
    ("expandtab", "et"),
    ("autoindent", "ai"),
    ("ignorecase", "ic"),
    ("smartcase", "scs"),
    ("hlsearch", "hls"),
    ("incsearch", "is"),
    ("scrolloff", "so"),
    ("timeout", "to"),
    ("timeoutlen", "tm"),
    ("path", "pa"),
    ("autoread", "ar"),
    ("writeany", "wa"),
    ("readonly", "ro"),
    ("swapfile", "swf"),
    ("filetype", "ft"),
    ("cdhome", "cdh"),
    ("cdpath", "cd"),
    ("viewdir", "vdir"),
    ("runtimepath", "rtp"),
    ("verbose", "vbs"),
    ("verbosefile", "vfile"),
];

/// Option names starting with the current completion `prefix`.
pub(super) fn candidates(prefix: &str) -> impl Iterator<Item = &'static str> + '_ {
    NAMES
        .iter()
        .map(|&(name, _)| name)
        .filter(move |name| name.starts_with(prefix))
}

impl Vim {
    /// Return display options for this buffer's view in `window`.
    pub fn view_options(&self, window: &Register) -> WindowOptions {
        self.view_options.unwrap_or_else(|| window.window_options())
    }

    /// Use the display options copied from a view when splitting its window.
    pub fn set_view_options(&mut self, options: WindowOptions) {
        self.view_options = Some(options);
    }

    /// Resolve the effective scroll margin of this buffer view in `window`.
    pub fn scrolloff(&self, window: &Register) -> usize {
        self.view_options(window)
            .scrolloff
            .unwrap_or_else(|| window.input_maps.with_options(|options| options.scrolloff))
    }

    /// Install shared defaults and seed local values on first entering a workspace.
    pub fn set_input_options(&mut self, mappings: &InputMappings) {
        if self.buffer_options.is_none() || !self.options.same_instance(mappings) {
            self.options = mappings.clone();
            if self.buffer_options.is_none() {
                let buffer = mappings.with_options(|options| options.buffer.clone());
                self.doc.set_tabstop(buffer.tabstop);
                self.buffer_options = Some(Arc::new(RwLock::new(buffer)));
            }
        }
    }

    /// Share options, tab geometry and mappings of another view of this buffer.
    pub fn share_buffer_options(&mut self, source: &Self) {
        self.buffer_options.clone_from(&source.buffer_options);
        self.doc.share_tabstop(&source.doc);
        self.share_buffer_mappings(source);
    }

    /// Keep private copies of editing options and mappings when a buffer leaves its Session.
    pub fn detach_buffer_options(&mut self) {
        self.buffer_options = Some(Arc::new(RwLock::new(self.buffer_options())));
        self.doc.detach_tabstop();
        self.detach_buffer_mappings();
    }

    /// Record the new tab spacing from :retab in this buffer's local options.
    pub(super) fn set_buffer_tabstop(&mut self, columns: usize) {
        let mut buffer = self.buffer_options.as_ref().unwrap().write().unwrap();
        buffer.tabstop = columns;
        buffer.local_defaults |= preferences::local_bit("tabstop");
        let origin = self.options.current_ex_origin();
        remember_origin(&mut buffer.origins, "tabstop", origin.as_ref());
        self.doc.set_tabstop(columns);
    }

    /// Snapshot the local indentation settings for this view.
    fn buffer_options(&self) -> BufferOptions {
        self.buffer_options
            .as_deref()
            .map_or_else(BufferOptions::default, |options| {
                options.read().unwrap().clone()
            })
    }

    /// Resolve the effective directory list for this buffer's :find commands.
    pub fn file_search_path(&self) -> String {
        self.buffer_options().path.as_deref().map_or_else(
            || self.options.with_options(|options| options.path.clone()),
            str::to_owned,
        )
    }

    /// Return whether a changed clean file should be reloaded without confirmation.
    pub fn autoread(&self) -> bool {
        self.buffer_options()
            .autoread
            .unwrap_or_else(|| self.options.with_options(|options| options.autoread))
    }

    /// Whether saving to the buffer's own filename requires an Ex bang.
    pub fn file_read_only(&self) -> bool {
        self.buffer_options().readonly
    }

    /// Return whether the shared buffer can write crash recovery checkpoints.
    pub fn swapfile_enabled(&self) -> bool {
        self.buffer_options.as_ref().map_or_else(
            || self.options.with_options(|options| options.buffer.swapfile),
            |options| options.read().unwrap().swapfile,
        )
    }

    /// Disable recovery for a buffer newly loaded under the :noswapfile modifier.
    pub fn disable_swapfile(&mut self) {
        if self.buffer_options.is_none() {
            self.set_input_options(&self.options.clone());
        }
        let mut buffer = self.buffer_options.as_ref().unwrap().write().unwrap();
        buffer.swapfile = false;
        buffer.local_defaults |= preferences::local_bit("swapfile");
        let origin = self.options.current_ex_origin();
        remember_origin(&mut buffer.origins, "swapfile", origin.as_ref());
    }

    /// Return this shared buffer's authoritative type override for language provider routing.
    pub fn filetype_override(&self) -> Option<String> {
        self.buffer_options
            .as_ref()?
            .read()
            .unwrap()
            .filetype
            .name
            .as_deref()
            .map(str::to_owned)
    }

    /// Return the assigned type or the Vim-level type inferred from this buffer's true identity.
    pub(crate) fn filetype(&self) -> String {
        if let Some(name) = self.filetype_override() {
            return name;
        }
        self.options
            .with_options(|options| options.filetype_detection)
            .then(|| super::ex_languages::infer(self.doc.path.as_deref(), &self.doc.text))
            .flatten()
            .unwrap_or_default()
            .to_owned()
    }

    /// Assign `name` unless a previous nonfallback assignment already owns the buffer's type.
    pub(super) fn assign_detected_filetype(&mut self, name: String, fallback: bool) {
        self.set_input_options(&self.options.clone());
        let filetype = {
            let buffer = self.buffer_options.as_ref().unwrap().read().unwrap();
            (buffer.filetype.name.is_none() || buffer.filetype.fallback).then_some(
                super::ex_languages::BufferFiletype {
                    name: Some(Arc::from(name)),
                    fallback,
                },
            )
        };
        if let Some(filetype) = filetype {
            self.replace_filetype(filetype);
        }
    }

    /// Rerun detection, supplying a language route when the real extension cannot select one.
    pub(super) fn redetect_filetype(&mut self) {
        let name = self.doc.path.as_deref().and_then(|path| {
            (path.extension().is_none() || super::ex_languages::infer(Some(path), "").is_none())
                .then(|| super::ex_languages::infer(Some(path), &self.doc.text))
                .flatten()
                .map(Arc::from)
        });
        self.replace_filetype(super::ex_languages::BufferFiletype {
            fallback: name.is_some(),
            name,
        });
    }

    /// Replace filetype state together with the shared language result version.
    fn replace_filetype(&mut self, filetype: super::ex_languages::BufferFiletype) {
        let changed = {
            let mut buffer = self.buffer_options.as_ref().unwrap().write().unwrap();
            let changed = buffer.filetype.name != filetype.name;
            buffer.filetype = filetype;
            let origin = self.options.current_ex_origin();
            remember_origin(&mut buffer.origins, "filetype", origin.as_ref());
            changed
        };
        if changed {
            self.doc.invalidate_language_dependents();
        }
    }

    /// Export the buffer and window options implemented by this editor as :setlocal lines.
    pub fn ex_view_option_lines(&self, register: &Register) -> Vec<String> {
        let mut options = self.options.options();
        let mut window = self.view_options(register);
        let mut buffer = self.buffer_options();
        let export_filetype = buffer.filetype.name.is_some();
        [
            "number",
            "relativenumber",
            "wrap",
            "scrolloff",
            "tabstop",
            "shiftwidth",
            "textwidth",
            "expandtab",
            "autoindent",
            "readonly",
            "swapfile",
            "filetype",
            "path",
            "autoread",
        ]
        .into_iter()
        .filter(|name| *name != "filetype" || export_filetype)
        .filter_map(|name| {
            set_option(
                name,
                Setting::Query,
                Scope::Local,
                self.markdown_wrap(),
                &mut options,
                &mut window,
                &mut buffer,
            )
            .ok()
            .flatten()
            .map(|value| format!("setlocal {}", ex_escape_option(&value)))
        })
        .collect()
    }

    /// Set the shared buffer's write protection independently of editing permissions.
    pub fn set_file_read_only(&mut self, read_only: bool) {
        if self.buffer_options.is_none() {
            self.set_input_options(&self.options.clone());
        }
        self.buffer_options
            .as_ref()
            .unwrap()
            .write()
            .unwrap()
            .readonly = read_only;
    }

    /// Effective indentation width, treating zero shiftwidth as tabstop.
    pub(super) fn shiftwidth(&self) -> usize {
        match self.buffer_options().shiftwidth {
            0 => self.doc.tabstop(),
            width => width,
        }
    }

    /// Whether typed tabs and indentation should become spaces.
    pub(super) fn expandtab(&self) -> bool {
        self.buffer_options().expandtab
    }

    /// Whether newly opened lines inherit indentation.
    pub(super) fn autoindent(&self) -> bool {
        self.buffer_options().autoindent
    }

    /// Buffer-local width used by :center and :right when they omit a target column.
    pub(super) fn textwidth(&self) -> usize {
        self.buffer_options().textwidth
    }

    /// Generate `columns` leading cells using this buffer's whitespace preferences.
    pub(super) fn indentation(&self, columns: usize) -> String {
        if self.expandtab() {
            " ".repeat(columns)
        } else {
            let tabstop = self.doc.tabstop();
            "\t".repeat(columns / tabstop) + &" ".repeat(columns % tabstop)
        }
    }

    /// Tell whether the current workspace previews searches before Enter.
    pub(super) fn incremental_search(&self) -> bool {
        self.options.with_options(|options| options.incsearch)
    }

    /// Tell whether the current workspace highlights accepted search results.
    pub(super) fn highlight_search(&self) -> bool {
        self.options.with_options(|options| options.hlsearch)
    }

    /// Resolve case sensitivity for `pattern`, preserving an explicit command override.
    pub(super) fn pattern_case(&self, pattern: &str, explicit: Option<bool>) -> Option<bool> {
        explicit.or_else(|| {
            Some(self.options.with_options(|options| {
                options.ignorecase
                    && !(options.smartcase && pattern.chars().any(char::is_uppercase))
            }))
        })
    }

    /// Apply a supported :set operation or a native :let mapleader assignment.
    pub(super) fn configure_options(&mut self, text: &str, register: &mut Register) -> bool {
        let (command, args) = text
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((text.trim(), ""));
        let args = args.trim();
        if command.len() >= 3 && "nohlsearch".starts_with(command) && args.is_empty() {
            self.search_highlights_hidden = true;
            self.message.clear();
            return true;
        }
        if command == "let" {
            let Some((name, value)) = args.split_once('=') else {
                return false;
            };
            if !matches!(name.trim(), "mapleader" | "g:mapleader") {
                return false;
            }
            match self.evaluate_expression(value.trim(), register) {
                Ok(super::expression::Value::String(value)) if valid_leader(&value) => {
                    let mut global = register.input_maps.options();
                    global.mapleader = value;
                    register.input_maps.set_options(global);
                }
                Ok(_) => self.option_error("mapleader"),
                Err(error) => {
                    self.ex_failed = true;
                    self.message = error;
                }
            }
            return true;
        }
        let scope = match command {
            "set" | "se" => Scope::Both,
            "setlocal" | "setl" => Scope::Local,
            "setglobal" | "setg" => Scope::Global,
            _ => return false,
        };
        self.set_input_options(&register.input_maps);
        let mut global = register.input_maps.options();
        global.verbose = register.input_maps.ex_verbose();
        let show_origins = global.verbose > 0;
        let mut window_defaults = global.window;
        global.window = register.window_options();
        let mut buffer = self.buffer_options();
        let mut window = self.view_options(register);
        let mut output = Vec::new();
        let tokens = (args != "all").then_some(args).unwrap_or_default();
        for (index, token) in option_words(tokens).enumerate() {
            let Some((name, setting)) = parse(token) else {
                if index == 0 {
                    return false;
                }
                self.option_error(token);
                return true;
            };
            let previous_filetype = buffer.filetype.name.clone();
            match set_option(
                name,
                setting,
                scope,
                self.markdown_wrap(),
                &mut global,
                &mut window,
                &mut buffer,
            ) {
                Ok(Some(mut value)) => {
                    if name == "filetype" && scope != Scope::Global {
                        value = format!("filetype={}", self.filetype());
                    }
                    if show_origins {
                        let origin = match (scope, name) {
                            (Scope::Global, _) => register.input_maps.ex_option_origin(name),
                            (_, "number" | "relativenumber" | "wrap" | "scrollbind") => {
                                self.view_option_origins.get(name).cloned()
                            }
                            (_, "scrolloff") if window.scrolloff.is_some() => {
                                self.view_option_origins.get(name).cloned()
                            }
                            (_, "path") if buffer.path.is_some() => {
                                buffer.origins.get(name).cloned()
                            }
                            (_, "autoread") if buffer.autoread.is_some() => {
                                buffer.origins.get(name).cloned()
                            }
                            (
                                _,
                                "tabstop" | "shiftwidth" | "textwidth" | "expandtab" | "autoindent"
                                | "readonly" | "swapfile" | "filetype",
                            ) => buffer.origins.get(name).cloned(),
                            _ => register.input_maps.ex_option_origin(name),
                        };
                        if let Some(origin) = &origin {
                            origin.append_inline(&mut value);
                        }
                    }
                    output.push(value);
                }
                Ok(None) => {
                    let origin = register.input_maps.current_ex_origin();
                    if scope != Scope::Global {
                        let mask = preferences::local_bit(name);
                        let explicit =
                            scope == Scope::Local && !matches!(setting, Setting::Inherit);
                        let flags = if matches!(
                            name,
                            "number" | "relativenumber" | "wrap" | "scrollbind"
                        ) {
                            &mut window.local_defaults
                        } else {
                            &mut buffer.local_defaults
                        };
                        if explicit {
                            *flags |= mask;
                        } else {
                            *flags &= !mask;
                        }
                    }
                    let is_window_local = matches!(
                        name,
                        "number" | "relativenumber" | "wrap" | "scrollbind" | "scrolloff"
                    );
                    let is_buffer_local = matches!(
                        name,
                        "tabstop"
                            | "shiftwidth"
                            | "textwidth"
                            | "expandtab"
                            | "autoindent"
                            | "path"
                            | "autoread"
                            | "readonly"
                            | "swapfile"
                            | "filetype"
                    );
                    if scope != Scope::Global && is_window_local {
                        remember_origin(&mut self.view_option_origins, name, origin.as_ref());
                    }
                    if scope != Scope::Global && is_buffer_local {
                        remember_origin(&mut buffer.origins, name, origin.as_ref());
                    }
                    if scope != Scope::Local && is_buffer_local {
                        remember_origin(&mut global.buffer.origins, name, origin.as_ref());
                    }
                    if scope != Scope::Local || !is_window_local && !is_buffer_local {
                        register.input_maps.set_ex_option_origin(name, origin);
                    }
                    match name {
                        "number" => window_defaults.number = global.window.number,
                        "relativenumber" => {
                            window_defaults.relativenumber = global.window.relativenumber
                        }
                        "wrap" => window_defaults.wrap = global.window.wrap,
                        "scrollbind" => window_defaults.scrollbind = global.window.scrollbind,
                        _ => (),
                    }
                    let mut updated = global.clone();
                    // Window-local defaults stay with their pane. Explicit global settings also
                    // seed subsequently created windows and the portable configuration snapshot.
                    updated.window = if scope == Scope::Local {
                        register.input_maps.with_options(|options| options.window)
                    } else {
                        window_defaults
                    };
                    register.input_maps.set_ex_options(updated);
                    register.set_window_options(global.window);
                    self.view_options = Some(window);
                    *self.buffer_options.as_ref().unwrap().write().unwrap() = buffer.clone();
                    self.doc.set_tabstop(buffer.tabstop);
                    if name == "filetype" && previous_filetype != buffer.filetype.name {
                        self.doc.invalidate_language_dependents();
                    }
                    if name == "hlsearch" {
                        self.search_highlights_hidden = false;
                    }
                }
                Err(()) => {
                    self.option_error(token);
                    return true;
                }
            }
        }
        if args.is_empty() || args == "all" {
            let mut defaults = VimOptions::default();
            let mut default_window = defaults.window;
            let mut default_buffer = defaults.buffer.clone();
            for &(name, _) in NAMES {
                let mut value = set_option(
                    name,
                    Setting::Query,
                    scope,
                    self.markdown_wrap(),
                    &mut global,
                    &mut window,
                    &mut buffer,
                )
                .unwrap();
                if name == "filetype" && scope != Scope::Global {
                    value = Some(format!("filetype={}", self.filetype()));
                }
                let default = set_option(
                    name,
                    Setting::Query,
                    scope,
                    self.markdown_wrap(),
                    &mut defaults,
                    &mut default_window,
                    &mut default_buffer,
                )
                .unwrap();
                if value != default || args == "all" {
                    output.extend(value);
                }
            }
        }
        self.message = output.join("  ");
        true
    }

    /// Report a mistyped option through the ordinary Ex status message.
    fn option_error(&mut self, token: &str) {
        self.ex_failed = true;
        self.message = format!("Invalid option: {token}");
    }

    /// The default wrap mode in the absence of an explicit setting.
    fn markdown_wrap(&self) -> bool {
        self.doc
            .path
            .as_deref()
            .and_then(std::path::Path::extension)
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|ext| {
                ["md", "markdown", "mdown", "mkd", "mkdn", "mdwn"]
                    .iter()
                    .any(|name| ext.eq_ignore_ascii_case(name))
            })
    }
}

/// Whether a literal string can be expanded into leader key tokens.
fn valid_leader(value: &str) -> bool {
    !value.is_empty() && !value.contains(['\0', '\r', '\n'])
}

/// Split :set arguments while retaining escaped whitespace inside option values.
fn option_words(text: &str) -> impl Iterator<Item = &str> {
    let mut escaping = false;
    text.split(move |ch: char| {
        let delimiter = ch.is_whitespace() && !escaping;
        escaping = ch == '\\' && !escaping;
        delimiter
    })
    .filter(|word| !word.is_empty())
}

/// Interpret an assignment, query, negation, inversion or reset independent of option type.
fn parse(token: &str) -> Option<(&'static str, Setting<'_>)> {
    let (name, setting) = if let Some((name, value)) = token.split_once(['=', ':']) {
        match name.as_bytes().last() {
            Some(b'+') => (&name[..name.len() - 1], Setting::Append(value)),
            Some(b'^') => (&name[..name.len() - 1], Setting::Prepend(value)),
            Some(b'-') => (&name[..name.len() - 1], Setting::Remove(value)),
            _ => (name, Setting::Assign(value)),
        }
    } else if let Some(name) = token.strip_suffix('?') {
        (name, Setting::Query)
    } else if let Some(name) = token
        .strip_suffix('!')
        .or_else(|| token.strip_prefix("inv"))
    {
        (name, Setting::Invert)
    } else if let Some(name) = token.strip_suffix('&') {
        (name, Setting::Reset)
    } else if let Some(name) = token.strip_suffix('<') {
        (name, Setting::Inherit)
    } else if let Some(name) = token.strip_prefix("no") {
        (name, Setting::Boolean(false))
    } else {
        (token, Setting::Boolean(true))
    };
    let name = NAMES
        .iter()
        .find(|&&(long, short)| name == long || !short.is_empty() && name == short)?
        .0;
    Some((name, setting))
}

/// Apply a parsed setting to the selected option's local and default values.
fn set_option(
    name: &str,
    setting: Setting<'_>,
    scope: Scope,
    markdown: bool,
    global: &mut VimOptions,
    window: &mut WindowOptions,
    buffer: &mut BufferOptions,
) -> Result<Option<String>, ()> {
    let defaults = VimOptions::default();
    match name {
        "number" => boolean(
            name,
            scope,
            setting,
            &mut global.window.number,
            Some(&mut window.number),
            defaults.window.number,
        ),
        "relativenumber" => boolean(
            name,
            scope,
            setting,
            &mut global.window.relativenumber,
            Some(&mut window.relativenumber),
            defaults.window.relativenumber,
        ),
        "scrollbind" => boolean(
            name,
            scope,
            setting,
            &mut global.window.scrollbind,
            Some(&mut window.scrollbind),
            defaults.window.scrollbind,
        ),
        "wrap" => {
            let old = if scope == Scope::Global {
                global.window.wrap
            } else {
                window.wrap
            };
            let value = match setting {
                Setting::Query => return Ok(Some(flag(name, old.unwrap_or(markdown)))),
                Setting::Boolean(value) => Some(value),
                Setting::Invert => Some(!old.unwrap_or(markdown)),
                Setting::Reset => None,
                Setting::Inherit => global.window.wrap,
                _ => return Err(()),
            };
            if scope != Scope::Local {
                global.window.wrap = value;
            }
            if scope != Scope::Global {
                window.wrap = value;
            }
            Ok(None)
        }
        "tabstop" => numeric(
            name,
            scope,
            setting,
            &mut global.buffer.tabstop,
            Some(&mut buffer.tabstop),
            defaults.buffer.tabstop,
            1..=MAX_COLUMNS,
        ),
        "shiftwidth" => numeric(
            name,
            scope,
            setting,
            &mut global.buffer.shiftwidth,
            Some(&mut buffer.shiftwidth),
            defaults.buffer.shiftwidth,
            0..=MAX_COLUMNS,
        ),
        "textwidth" => numeric(
            name,
            scope,
            setting,
            &mut global.buffer.textwidth,
            Some(&mut buffer.textwidth),
            defaults.buffer.textwidth,
            0..=MAX_COLUMNS,
        ),
        "expandtab" => boolean(
            name,
            scope,
            setting,
            &mut global.buffer.expandtab,
            Some(&mut buffer.expandtab),
            defaults.buffer.expandtab,
        ),
        "autoindent" => boolean(
            name,
            scope,
            setting,
            &mut global.buffer.autoindent,
            Some(&mut buffer.autoindent),
            defaults.buffer.autoindent,
        ),
        "ignorecase" => boolean(
            name,
            scope,
            setting,
            &mut global.ignorecase,
            None,
            defaults.ignorecase,
        ),
        "smartcase" => boolean(
            name,
            scope,
            setting,
            &mut global.smartcase,
            None,
            defaults.smartcase,
        ),
        "incsearch" => boolean(
            name,
            scope,
            setting,
            &mut global.incsearch,
            None,
            defaults.incsearch,
        ),
        "hlsearch" => boolean(
            name,
            scope,
            setting,
            &mut global.hlsearch,
            None,
            defaults.hlsearch,
        ),
        "timeout" => boolean(
            name,
            scope,
            setting,
            &mut global.timeout,
            None,
            defaults.timeout,
        ),
        "timeoutlen" | "verbose" => {
            let (value, default) = if name == "verbose" {
                (&mut global.verbose, defaults.verbose)
            } else {
                (&mut global.timeoutlen, defaults.timeoutlen)
            };
            let mut current = *value as usize;
            let report = numeric(
                name,
                scope,
                setting,
                &mut current,
                None,
                default as usize,
                0..=u32::MAX as usize,
            )?;
            *value = current as u32;
            Ok(report)
        }
        "path" => {
            let current = if scope == Scope::Global {
                &global.path
            } else {
                buffer.path.as_deref().unwrap_or(&global.path)
            };
            let value = match setting {
                Setting::Query | Setting::Boolean(true) => {
                    return Ok(Some(format!("path={current}")));
                }
                Setting::Assign(value) => value.to_owned(),
                Setting::Append(value) => join_path_list(current, value),
                Setting::Prepend(value) => join_path_list(value, current),
                Setting::Remove(value) => current
                    .split(',')
                    .filter(|part| !value.split(',').any(|removed| removed == *part))
                    .collect::<Vec<_>>()
                    .join(","),
                Setting::Reset => defaults.path,
                Setting::Inherit => global.path.clone(),
                _ => return Err(()),
            };
            if scope != Scope::Local {
                global.path = value.clone();
            }
            if scope != Scope::Global {
                buffer.path = (scope == Scope::Local
                    && !value.is_empty()
                    && !matches!(setting, Setting::Inherit))
                .then(|| Arc::from(value));
            }
            Ok(None)
        }
        "autoread" => {
            let mut local = buffer.autoread.unwrap_or(global.autoread);
            let result = boolean(
                name,
                scope,
                setting,
                &mut global.autoread,
                Some(&mut local),
                defaults.autoread,
            )?;
            if scope != Scope::Global && result.is_none() {
                buffer.autoread = (scope == Scope::Local && !matches!(setting, Setting::Inherit))
                    .then_some(local);
            }
            Ok(result)
        }
        "writeany" => boolean(
            name,
            scope,
            setting,
            &mut global.writeany,
            None,
            defaults.writeany,
        ),
        "readonly" => boolean(
            name,
            scope,
            setting,
            &mut false,
            Some(&mut buffer.readonly),
            false,
        ),
        "swapfile" => boolean(
            name,
            scope,
            setting,
            &mut global.buffer.swapfile,
            Some(&mut buffer.swapfile),
            defaults.buffer.swapfile,
        ),
        "filetype" => {
            if matches!(setting, Setting::Query | Setting::Boolean(true)) {
                let current = if scope == Scope::Global {
                    ""
                } else {
                    buffer.filetype.name.as_deref().unwrap_or_default()
                };
                return Ok(Some(format!("filetype={current}")));
            }
            if scope == Scope::Global {
                return Err(());
            }
            let name = match setting {
                Setting::Assign("") | Setting::Inherit | Setting::Reset => None,
                Setting::Assign(value) => {
                    Some(Arc::from(super::ex_languages::normalize(value).ok_or(())?))
                }
                _ => return Err(()),
            };
            buffer.filetype = super::ex_languages::BufferFiletype {
                name,
                fallback: false,
            };
            Ok(None)
        }
        "cdhome" => boolean(
            name,
            scope,
            setting,
            &mut global.cdhome,
            None,
            defaults.cdhome,
        ),
        "cdpath" => {
            let value = match setting {
                Setting::Query | Setting::Boolean(true) => {
                    return Ok(Some(format!("cdpath={}", global.cdpath)));
                }
                Setting::Assign(value) => value.to_owned(),
                Setting::Append(value) => join_path_list(&global.cdpath, value),
                Setting::Prepend(value) => join_path_list(value, &global.cdpath),
                Setting::Remove(value) => global
                    .cdpath
                    .split(',')
                    .filter(|part| !value.split(',').any(|removed| removed == *part))
                    .collect::<Vec<_>>()
                    .join(","),
                Setting::Reset => defaults.cdpath,
                Setting::Inherit => global.cdpath.clone(),
                _ => return Err(()),
            };
            global.cdpath = value;
            Ok(None)
        }
        "viewdir" => {
            global.viewdir = match setting {
                Setting::Query | Setting::Boolean(true) => {
                    return Ok(Some(format!("viewdir={}", global.viewdir)));
                }
                Setting::Assign(value) => value.to_owned(),
                Setting::Reset => defaults.viewdir,
                Setting::Inherit => return Ok(None),
                _ => return Err(()),
            };
            Ok(None)
        }
        "runtimepath" => {
            global.runtimepath = match setting {
                Setting::Query | Setting::Boolean(true) => {
                    return Ok(Some(format!("runtimepath={}", global.runtimepath)));
                }
                Setting::Assign(value) => value.to_owned(),
                Setting::Append(value) => join_path_list(&global.runtimepath, value),
                Setting::Prepend(value) => join_path_list(value, &global.runtimepath),
                Setting::Remove(value) => global
                    .runtimepath
                    .split(',')
                    .filter(|part| !value.split(',').any(|removed| removed == *part))
                    .collect::<Vec<_>>()
                    .join(","),
                Setting::Reset => defaults.runtimepath,
                Setting::Inherit => return Ok(None),
                _ => return Err(()),
            };
            Ok(None)
        }
        "verbosefile" => {
            global.verbosefile = match setting {
                Setting::Query | Setting::Boolean(true) => {
                    return Ok(Some(format!("verbosefile={}", global.verbosefile)));
                }
                Setting::Assign(value) => value.replace("\\ ", " "),
                Setting::Append(value) => global.verbosefile.clone() + &value.replace("\\ ", " "),
                Setting::Prepend(value) => value.replace("\\ ", " ") + &global.verbosefile,
                Setting::Remove(value) => {
                    global
                        .verbosefile
                        .replacen(&value.replace("\\ ", " "), "", 1)
                }
                Setting::Reset => defaults.verbosefile,
                Setting::Inherit => return Ok(None),
                _ => return Err(()),
            };
            Ok(None)
        }
        "scrolloff" => {
            if matches!(setting, Setting::Inherit | Setting::Assign("-1")) && scope == Scope::Local
            {
                window.scrolloff = None;
                return Ok(None);
            }
            let mut local = window.scrolloff.unwrap_or(global.scrolloff);
            let report = numeric(
                name,
                scope,
                setting,
                &mut global.scrolloff,
                Some(&mut local),
                defaults.scrolloff,
                0..=usize::MAX,
            )?;
            if report.is_none() && scope != Scope::Global {
                window.scrolloff = (scope == Scope::Local).then_some(local);
            }
            Ok(report)
        }
        _ => Err(()),
    }
}

/// An assignment made directly in the editor clears the previous script location.
fn remember_origin(
    origins: &mut BTreeMap<&'static str, ExOrigin>,
    name: &'static str,
    origin: Option<&ExOrigin>,
) {
    if let Some(origin) = origin {
        origins.insert(name, origin.clone());
    } else {
        origins.remove(name);
    }
}

/// Format one boolean in the usual Vim option listing notation.
fn flag(name: &str, value: bool) -> String {
    if value {
        name.to_owned()
    } else {
        format!("no{name}")
    }
}

/// Read or set a boolean with global and optional local storage.
fn boolean(
    name: &str,
    scope: Scope,
    setting: Setting<'_>,
    global: &mut bool,
    local: Option<&mut bool>,
    default: bool,
) -> Result<Option<String>, ()> {
    let current = effective(scope, *global, local.as_deref().copied());
    let value = match setting {
        Setting::Query => return Ok(Some(flag(name, current))),
        Setting::Boolean(value) => value,
        Setting::Invert => !current,
        Setting::Reset => default,
        Setting::Inherit => *global,
        _ => return Err(()),
    };
    write_scoped(scope, global, local, value);
    Ok(None)
}

/// Append one search path list to another, joining only nonempty lists.
fn join_path_list(first: &str, second: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (true, _) => second.to_owned(),
        (_, true) => first.to_owned(),
        _ => format!("{first},{second}"),
    }
}

/// Keep an option value containing spaces together as one Ex word.
fn ex_escape_option(value: &str) -> String {
    value.replace(' ', "\\ ")
}

/// Parse and validate an integer before assigning it to the selected scope.
fn numeric(
    name: &str,
    scope: Scope,
    setting: Setting<'_>,
    global: &mut usize,
    local: Option<&mut usize>,
    default: usize,
    allowed: std::ops::RangeInclusive<usize>,
) -> Result<Option<String>, ()> {
    let value = match setting {
        Setting::Boolean(true) | Setting::Query => {
            return Ok(Some(format!(
                "{name}={}",
                effective(scope, *global, local.as_deref().copied())
            )));
        }
        Setting::Assign(value) => value
            .parse()
            .ok()
            .filter(|value| allowed.contains(value))
            .ok_or(())?,
        Setting::Reset => default,
        Setting::Inherit => *global,
        _ => return Err(()),
    };
    write_scoped(scope, global, local, value);
    Ok(None)
}

/// Use the global `default` for a global query or in the absence of a local value.
fn effective<T: Copy>(scope: Scope, default: T, local: Option<T>) -> T {
    if scope == Scope::Global {
        default
    } else {
        local.unwrap_or(default)
    }
}

/// Store a validated value in the scopes selected by an Ex setting command.
fn write_scoped<T: Copy>(scope: Scope, global: &mut T, local: Option<&mut T>, value: T) {
    match local {
        Some(local) => {
            if scope != Scope::Global {
                *local = value;
            }
            if scope != Scope::Local {
                *global = value;
            }
        }
        None => *global = value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn command(vim: &mut Vim, register: &mut Register, input: &str) {
        vim.text(":", register);
        vim.text(input, register);
        vim.key("Enter", register);
    }

    #[test]
    fn setlocal_changes_only_the_current_window_and_buffer() {
        let default = InputMappings::default();
        let mut first = Register::default();
        first.set_input_mappings(default.clone());
        let mut vim = Vim::new(Document::new("x"));
        command(&mut vim, &mut first, "setlocal nonumber et ts=2");
        assert!(!vim.view_options(&first).number);
        vim.key("A", &mut first);
        vim.key("Tab", &mut first);
        vim.key("Escape", &mut first);
        assert_eq!(vim.doc.text, "x ");

        let mut second = Register::default();
        second.set_input_mappings(default);
        let mut second_view = Vim::new(Document::new("x"));
        second_view.share_buffer_options(&vim);
        assert!(second_view.view_options(&second).number);
        assert_eq!(second_view.doc.tabstop(), 2);
    }

    /// Swapfile settings are shared by buffer views and use separate defaults for new buffers.
    #[test]
    fn swapfile_option_is_shared_by_views_and_keeps_global_defaults_separate() {
        let mut register = Register::default();
        let mut file = Vim::new(Document::new("file"));
        command(&mut file, &mut register, "setlocal swf?");
        assert_eq!(file.message, "swapfile");
        let mut second_view = Vim::new(Document::new("file"));
        second_view.share_buffer_options(&file);
        file.disable_swapfile();
        assert!(!file.swapfile_enabled());
        assert!(!second_view.swapfile_enabled());

        command(&mut file, &mut register, "setlocal swf");
        command(&mut file, &mut register, "setglobal noswapfile");
        assert!(second_view.swapfile_enabled());
        let mut other = Vim::new(Document::new("other"));
        other.set_input_options(&register.input_maps);
        assert!(!other.swapfile_enabled());
        command(&mut file, &mut register, "setlocal swapfile<");
        assert!(!file.swapfile_enabled());
        assert!(!second_view.swapfile_enabled());
    }

    #[test]
    fn window_display_defaults_seed_later_files_without_copying_local_overrides() {
        let mut register = Register::default();
        let mut file = Vim::new(Document::new("first"));
        command(&mut file, &mut register, "set nowrap");
        command(&mut file, &mut register, "setlocal wrap nonumber");
        assert_eq!(file.view_options(&register).wrap, Some(true));
        assert!(!file.view_options(&register).number);
        let mut other = Vim::new(Document::new("second"));
        command(&mut other, &mut register, "set wrap?");
        assert_eq!(other.message, "nowrap");
        assert!(other.view_options(&register).number);
    }

    #[test]
    fn setglobal_tabstop_only_changes_later_buffers() {
        let mut register = Register::default();
        let mut existing = Vim::new(Document::new("\tx"));
        command(&mut existing, &mut register, "setglobal ts=8");
        assert_eq!(existing.doc.location(1), (0, 4));
        let mut next = Vim::new(Document::new("\tx"));
        command(&mut next, &mut register, "set ts?");
        assert_eq!(next.doc.location(1), (0, 8));
        assert_eq!(next.message, "tabstop=8");
    }

    #[test]
    fn search_options_change_jumps_and_highlights() {
        let mut register = Register::default();
        let mut vim = Vim::new(Document::new("_ Name name"));
        command(&mut vim, &mut register, "set ignorecase smartcase hlsearch");
        vim.text("/name", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.doc.cursor, 2);
        assert_eq!(vim.search_highlights(0..vim.doc.text.len()), [2..6, 7..11]);

        command(&mut vim, &mut register, "noh");
        assert!(vim.search_highlights(0..vim.doc.text.len()).is_empty());
        vim.text("/Name", &mut register);
        vim.key("Enter", &mut register);
        assert_eq!(vim.search_highlights(0..vim.doc.text.len()), [2..6]);
    }
}
