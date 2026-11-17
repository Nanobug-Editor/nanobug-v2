//! Vim menu definitions and invocation, independent of their native presentation.

use super::{Action, Mode, Pending, Register, ScreenMotionResolver, Vim};
use crate::i18n;
use std::collections::BTreeMap;

/// One visible child of the menu currently open in the native window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuEntry {
    /// Display name without Vim's ampersand shortcut marker.
    pub label: String,
    /// Canonical name components that identify this entry.
    pub path: Vec<String>,
    /// Whether this entry leads to another menu.
    pub submenu: bool,
    /// Whether the entry can be activated in the current mode.
    pub enabled: bool,
    /// Whether the item represents a nonselectable menu divider.
    pub separator: bool,
    /// Optional shortcut demonstrated after the name in the definition.
    pub accelerator: Option<String>,
    /// Optional letter to activate the entry from its parent menu.
    pub mnemonic: Option<char>,
    /// Help text shown while the entry is highlighted in the popup.
    pub tooltip: Option<String>,
}

/// Selection state retained when entering a command-line popup or menu invocation.
#[derive(Clone, Copy)]
pub(super) struct Origin {
    /// Visual shape of the retained selection.
    mode: Mode,
    /// Fixed endpoint of the selection.
    anchor: usize,
    /// Moving endpoint of the selection.
    caret: usize,
    /// Whether the area accepts Select-mode replacement.
    select: bool,
}

/// Independent slots used by the Ex menu command family.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum MenuMode {
    Normal,
    Visual,
    Select,
    Operator,
    Insert,
    Command,
    Terminal,
}

/// Modes affected by unqualified :menu, :menu! and :amenu, respectively.
const MENU: &[MenuMode] = &[
    MenuMode::Normal,
    MenuMode::Visual,
    MenuMode::Select,
    MenuMode::Operator,
];
const MENU_BANG: &[MenuMode] = &[MenuMode::Insert, MenuMode::Command];
const AMENU: &[MenuMode] = &[
    MenuMode::Normal,
    MenuMode::Visual,
    MenuMode::Select,
    MenuMode::Operator,
    MenuMode::Insert,
    MenuMode::Command,
];

impl MenuMode {
    /// Select the binding associated with an explicit `:emenu` mode `name`.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "n" => Self::Normal,
            "v" => Self::Visual,
            "s" => Self::Select,
            "o" => Self::Operator,
            "i" => Self::Insert,
            "c" => Self::Command,
            "t" => Self::Terminal,
            _ => return None,
        })
    }

    /// Short Vim mode designator used in :menu output.
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "n",
            Self::Visual => "v",
            Self::Select => "s",
            Self::Operator => "o",
            Self::Insert => "i",
            Self::Command => "c",
            Self::Terminal => "t",
        }
    }
}

/// Workspace menu tree, kept alongside shared keyboard mappings.
#[derive(Default)]
pub(super) struct Menus {
    roots: Vec<Node>,
    /// Labels that should be translated when a menu entry is subsequently defined.
    translations: BTreeMap<String, Label>,
}

/// Ordered name, children and per-mode action for one tree node.
struct Node {
    label: Label,
    /// The original label remains valid for Ex commands and completions.
    original: String,
    priority: u32,
    children: Vec<Node>,
    bindings: [Option<Binding>; 7],
    /// Menu help belongs to the entry independently of its mode bindings.
    tooltip: Option<String>,
}

/// Leaf action installed by one menu command for one edit mode.
#[derive(Clone)]
struct Binding {
    /// Mapping RHS in Vim key notation.
    replacement: String,
    /// Allow remapping the RHS as it runs.
    recursive: bool,
    /// Use Visual commands and resume Select at the end when applicable.
    restore_select: bool,
    /// Whether the command is currently enabled in its installed mode.
    enabled: bool,
}

/// A parsed, unescaped menu segment and its optional presentation metadata.
#[derive(Clone)]
struct Label {
    /// Unescaped label including mnemonic and shortcut notation.
    raw: String,
    /// Display text with shortcut notation removed and literal ampersands preserved.
    name: String,
    /// Optional shortcut description from the <Tab> suffix.
    accelerator: Option<String>,
    /// Optional mnemonic named by an ampersand.
    mnemonic: Option<char>,
}

impl Label {
    /// Decode the mnemonic and accelerator from one unescaped `name`.
    fn parse(name: &str) -> Self {
        let raw = name.to_owned();
        let tab = name.find('\t').map(|index| (index, 1)).or_else(|| {
            name.as_bytes()
                .windows(5)
                .position(|part| part.eq_ignore_ascii_case(b"<tab>"))
                .map(|index| (index, 5))
        });
        let (name, accelerator) = tab.map_or((name, None), |(index, size)| {
            (&name[..index], Some(name[index + size..].to_owned()))
        });
        let mut display = String::new();
        let mut mnemonic = None;
        let mut chars = name.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '&'
                && let Some(next) = chars.next()
            {
                if next != '&' && mnemonic.is_none() {
                    mnemonic = Some(next);
                }
                display.push(next);
            } else {
                display.push(ch);
            }
        }
        Self {
            raw,
            name: display,
            accelerator,
            mnemonic,
        }
    }

    /// Whether the label is reserved for a nonselectable divider.
    fn is_separator(&self) -> bool {
        self.name.starts_with('-') && self.name.ends_with('-')
    }
}

impl Node {
    /// Accept the original menu name as well as the displayed translation.
    fn matches(&self, name: &str) -> bool {
        self.label.name == name || self.original == name
    }

    /// Determine whether this entry or a descendant is available in `mode`.
    fn available(&self, mode: MenuMode) -> bool {
        self.bindings[mode as usize]
            .as_ref()
            .is_some_and(|binding| binding.enabled)
            || self.children.iter().any(|node| node.available(mode))
    }
}

impl Menus {
    /// Find siblings in the submenu at `path`.
    fn children(&self, path: &[String]) -> Option<&[Node]> {
        let mut nodes = self.roots.as_slice();
        for name in path {
            nodes = &nodes.iter().find(|node| node.matches(name))?.children;
        }
        Some(nodes)
    }

    /// Find the exact leaf at `path` before dispatching its action.
    fn node(&self, path: &[String]) -> Option<&Node> {
        let (name, parent) = path.split_last()?;
        self.children(parent)?
            .iter()
            .find(|node| node.matches(name))
    }

    /// Present the children of `path` and whether they can run in `mode`.
    fn entries(&self, path: &[String], mode: MenuMode) -> Vec<MenuEntry> {
        self.children(path)
            .unwrap_or_default()
            .iter()
            .filter(|node| !path.is_empty() || !node.label.name.starts_with(']'))
            .map(|node| {
                let separator = node.label.is_separator();
                let mut path = path.to_vec();
                path.push(node.label.name.clone());
                MenuEntry {
                    label: node.label.name.clone(),
                    path,
                    submenu: !node.children.is_empty(),
                    enabled: !separator && node.available(mode),
                    separator,
                    accelerator: node.label.accelerator.clone(),
                    mnemonic: node.label.mnemonic,
                    tooltip: node.tooltip.clone(),
                }
            })
            .collect()
    }

    /// Install `bindings` for a new or existing leaf named by `labels`, sorted by `priority`.
    fn define(
        &mut self,
        labels: Vec<Label>,
        priority: &[u32],
        bindings: impl IntoIterator<Item = (MenuMode, Binding)>,
    ) -> Result<(), &'static str> {
        if labels.len() < 2 || labels[..labels.len() - 1].iter().any(Label::is_separator) {
            return Err("vim_menu.invalid_path");
        }
        let labels = labels
            .into_iter()
            .map(|label| {
                let original = label.name.clone();
                let localized = self
                    .translations
                    .get(&label.raw.to_lowercase())
                    .cloned()
                    .unwrap_or(label);
                (original, localized)
            })
            .collect::<Vec<_>>();
        let mut existing = self.roots.as_slice();
        for (index, (original, label)) in labels.iter().enumerate() {
            let Some(node) = existing
                .iter()
                .find(|node| node.matches(original) || node.matches(&label.name))
            else {
                break;
            };
            let leaf = node.children.is_empty();
            if leaf != (index + 1 == labels.len()) {
                return Err("vim_menu.path_conflict");
            }
            existing = &node.children;
        }
        let mut nodes = &mut self.roots;
        let last = labels.len() - 1;
        for (index, (original, label)) in labels.into_iter().enumerate() {
            let at = nodes
                .iter()
                .position(|node| node.matches(&original) || node.matches(&label.name))
                .unwrap_or_else(|| {
                    let priority = priority.get(index).copied().unwrap_or(500);
                    let at = nodes.partition_point(|node| node.priority <= priority);
                    nodes.insert(
                        at,
                        Node {
                            label,
                            original,
                            priority,
                            children: Vec::new(),
                            bindings: Default::default(),
                            tooltip: None,
                        },
                    );
                    at
                });
            let node = &mut nodes[at];
            if index == last {
                for (mode, binding) in bindings {
                    node.bindings[mode as usize] = Some(binding);
                }
                break;
            }
            nodes = &mut node.children;
        }
        Ok(())
    }

    /// Remove or enable matching `modes` under `path`, returning whether an entry matched.
    fn edit(&mut self, path: &[String], modes: &[MenuMode], edit: Option<bool>) -> bool {
        fn update(
            nodes: &mut Vec<Node>,
            path: &[String],
            modes: &[MenuMode],
            edit: Option<bool>,
        ) -> bool {
            let mut found = false;
            for node in nodes.iter_mut() {
                let remaining = match path.split_first() {
                    Some((head, rest)) if node.matches(head) => rest,
                    Some(_) => continue,
                    None => path,
                };
                if !node.children.is_empty() {
                    found |= update(&mut node.children, remaining, modes, edit);
                } else if remaining.is_empty() {
                    for &mode in modes {
                        let binding = &mut node.bindings[mode as usize];
                        if let Some(previous) = binding {
                            found = true;
                            if let Some(enabled) = edit {
                                previous.enabled = enabled;
                            } else {
                                *binding = None;
                            }
                        }
                    }
                }
            }
            nodes.retain(|node| {
                !node.children.is_empty() || node.bindings.iter().any(Option::is_some)
            });
            found
        }
        update(&mut self.roots, path, modes, edit)
    }

    /// List mapped paths and mode-specific right sides under `path`.
    fn listing(&self, path: &[String], modes: &[MenuMode]) -> String {
        fn append(
            nodes: &[Node],
            path: &mut Vec<String>,
            modes: &[MenuMode],
            output: &mut Vec<String>,
        ) {
            for node in nodes {
                path.push(node.label.name.clone());
                if node.children.is_empty() {
                    for &mode in modes {
                        if let Some(binding) = &node.bindings[mode as usize] {
                            let mut flags = String::new();
                            if !binding.recursive {
                                flags.push('*');
                            }
                            if !binding.enabled {
                                flags.push('-');
                            }
                            output.push(format!(
                                "{} {} {}{}",
                                path.join("."),
                                mode.name(),
                                flags,
                                binding.replacement
                            ));
                        }
                    }
                } else {
                    append(&node.children, path, modes, output);
                }
                path.pop();
            }
        }
        let mut output = Vec::new();
        if path.is_empty() {
            append(&self.roots, &mut Vec::new(), modes, &mut output);
        } else if let Some(node) = self.node(path) {
            append(
                std::slice::from_ref(node),
                &mut path[..path.len() - 1].to_vec(),
                modes,
                &mut output,
            );
        }
        output.join("  ·  ")
    }

    /// Change the tip of an existing menu entry, optionally removing tips from its descendants.
    fn change_tip(&mut self, path: &[String], tip: Option<&str>) -> bool {
        fn remove_all(node: &mut Node) -> bool {
            let mut changed = node.tooltip.take().is_some();
            for child in &mut node.children {
                changed |= remove_all(child);
            }
            changed
        }

        let mut nodes = &mut self.roots;
        let Some((last, parents)) = path.split_last() else {
            return false;
        };
        for parent in parents {
            let Some(node) = nodes.iter_mut().find(|node| node.matches(parent)) else {
                return false;
            };
            nodes = &mut node.children;
        }
        if last == "*" && tip.is_none() {
            let mut changed = false;
            for node in nodes {
                changed |= remove_all(node);
            }
            return changed;
        }
        let Some(node) = nodes.iter_mut().find(|node| node.matches(last)) else {
            return false;
        };
        if let Some(tip) = tip {
            node.tooltip = Some(tip.into());
            true
        } else {
            remove_all(node)
        }
    }

    /// List tip labels and messages under an optional menu path.
    fn tip_listing(&self, path: &[String]) -> String {
        fn append(nodes: &[Node], path: &mut Vec<String>, output: &mut Vec<String>) {
            for node in nodes {
                path.push(node.label.name.clone());
                if let Some(tip) = &node.tooltip {
                    output.push(format!("{}  {tip}", path.join(".")));
                }
                append(&node.children, path, output);
                path.pop();
            }
        }

        let mut output = Vec::new();
        if path.is_empty() {
            append(&self.roots, &mut Vec::new(), &mut output);
        } else if let Some(node) = self.node(path) {
            append(
                std::slice::from_ref(node),
                &mut path[..path.len() - 1].to_vec(),
                &mut output,
            );
        }
        output.join("\n")
    }

    /// Gather escaped Ex menu path completions that start with `prefix`.
    fn candidates(&self, prefix: &str, submenus: bool) -> Vec<String> {
        fn visit(
            nodes: &[Node],
            parent: &str,
            submenus: bool,
            prefix: &str,
            output: &mut Vec<String>,
        ) {
            for node in nodes {
                if node.label.is_separator() {
                    continue;
                }
                let name = parent.to_owned() + &escape_name(&node.label.name);
                if name.starts_with(prefix) && (!submenus || !node.children.is_empty()) {
                    output.push(name.clone());
                }
                if !node.children.is_empty() {
                    visit(&node.children, &(name + "."), submenus, prefix, output);
                }
            }
        }
        let mut output = Vec::new();
        visit(&self.roots, "", submenus, prefix, &mut output);
        output
    }
}

/// A recognized mapping configuration command and its implied edit modes.
#[derive(Clone, Copy)]
struct MenuCommand {
    modes: &'static [MenuMode],
    operation: Operation,
    all_modes: bool,
}

/// Whether the recognized Ex command defines or removes a menu.
#[derive(Clone, Copy)]
enum Operation {
    Define { recursive: bool },
    Remove,
}

/// Full spellings and minimum accepted abbreviations for menu configuration commands.
const COMMANDS: &[(&str, usize, MenuCommand)] = {
    use MenuMode::*;
    use Operation::*;
    const fn def(modes: &'static [MenuMode], recursive: bool, all_modes: bool) -> MenuCommand {
        MenuCommand {
            modes,
            operation: Define { recursive },
            all_modes,
        }
    }
    const fn rm(modes: &'static [MenuMode]) -> MenuCommand {
        MenuCommand {
            modes,
            operation: Remove,
            all_modes: false,
        }
    }
    &[
        ("menu", 2, def(MENU, true, false)),
        ("noremenu", 6, def(MENU, false, false)),
        ("unmenu", 4, rm(MENU)),
        ("amenu", 2, def(AMENU, true, true)),
        ("anoremenu", 2, def(AMENU, false, true)),
        ("aunmenu", 3, rm(AMENU)),
        ("nmenu", 3, def(&[Normal], true, false)),
        ("nnoremenu", 7, def(&[Normal], false, false)),
        ("nunmenu", 5, rm(&[Normal])),
        ("vmenu", 3, def(&[Visual, Select], true, false)),
        ("vnoremenu", 7, def(&[Visual, Select], false, false)),
        ("vunmenu", 5, rm(&[Visual, Select])),
        ("xmenu", 3, def(&[Visual], true, false)),
        ("xnoremenu", 7, def(&[Visual], false, false)),
        ("xunmenu", 5, rm(&[Visual])),
        ("smenu", 3, def(&[Select], true, false)),
        ("snoremenu", 7, def(&[Select], false, false)),
        ("sunmenu", 5, rm(&[Select])),
        ("omenu", 3, def(&[Operator], true, false)),
        ("onoremenu", 7, def(&[Operator], false, false)),
        ("ounmenu", 5, rm(&[Operator])),
        ("imenu", 3, def(&[Insert], true, false)),
        ("inoremenu", 7, def(&[Insert], false, false)),
        ("iunmenu", 5, rm(&[Insert])),
        ("cmenu", 3, def(&[Command], true, false)),
        ("cnoremenu", 7, def(&[Command], false, false)),
        ("cunmenu", 5, rm(&[Command])),
        ("tlmenu", 3, def(&[Terminal], true, false)),
        ("tlnoremenu", 3, def(&[Terminal], false, false)),
        ("tlunmenu", 3, rm(&[Terminal])),
    ]
};

/// Determine the installed binding family from the typed Ex command `name`.
fn parse_command(name: &str) -> Option<MenuCommand> {
    let (name, bang) = name
        .strip_suffix('!')
        .map_or((name, false), |name| (name, true));
    let mut command = COMMANDS
        .iter()
        .find(|(word, min, _)| *min <= name.len() && word.starts_with(name))?
        .2;
    if bang {
        if command.modes != MENU {
            return None;
        }
        command.modes = MENU_BANG;
    }
    Some(command)
}

/// Recognize menu tip definitions and removals, including their native abbreviations.
fn tip_command(name: &str) -> Option<bool> {
    match name {
        name if name.starts_with("tm") && "tmenu".starts_with(name) => Some(false),
        name if name.starts_with("tu") && "tunmenu".starts_with(name) => Some(true),
        _ => None,
    }
}

/// Complete canonical Ex menu verbs for the requested `prefix`.
pub(super) fn ex_candidates(prefix: &str) -> impl Iterator<Item = String> + '_ {
    COMMANDS
        .iter()
        .map(|(name, _, _)| *name)
        .chain(["emenu", "popup", "tmenu", "tunmenu"])
        .filter_map(move |name| {
            if name.starts_with(prefix) {
                Some(name.into())
            } else if let Some(prefix) = prefix.strip_suffix('!')
                && matches!(name, "menu" | "noremenu" | "unmenu" | "popup")
                && name.starts_with(prefix)
            {
                Some(format!("{name}!"))
            } else {
                None
            }
        })
}

/// Complete the menu path argument in `args` for a supported Ex menu `verb`.
pub(super) fn complete_argument<'a>(
    verb: &str,
    args: &'a str,
    register: &Register,
) -> Option<(&'a str, Vec<String>)> {
    let popup = is_popup(verb);
    let execute = is_execute(verb);
    if !popup && !execute && parse_command(verb).is_none() && tip_command(verb).is_none() {
        return None;
    }
    let args = args.trim_start();
    let (mode, rest) = split_word(args);
    let args = if execute && MenuMode::from_name(mode).is_some() && args.len() > mode.len() {
        rest
    } else {
        args
    };
    let candidates = register
        .input_maps
        .menus(|menu| menu.candidates(args, popup));
    Some((args, candidates))
}

impl Vim {
    /// Apply a translation to a future menu label, or clear the translation table.
    pub(super) fn translate_ex_menu(&mut self, args: &str, register: &Register) -> Action {
        if args == "clear" {
            register
                .input_maps
                .change_menus(|menu| menu.translations.clear());
            return Action::None;
        }
        let labels = || {
            let (mut original, rest) = parse_path(args).ok()?;
            let (mut translation, rest) = parse_path(rest).ok()?;
            if original.len() == 1 && translation.len() == 1 && rest.is_empty() {
                Some((original.pop()?.raw.to_lowercase(), translation.pop()?))
            } else {
                None
            }
        };
        if let Some((key, label)) = labels() {
            register
                .input_maps
                .change_menus(|menu| menu.translations.insert(key, label));
        } else {
            self.ex_failed = true;
            self.message = i18n::text("vim_menu.invalid_path").into();
        }
        Action::None
    }

    /// Return the current Visual or Select state for a command-line or script invocation.
    pub(super) fn menu_origin(&self) -> Option<Origin> {
        self.mode.is_visual().then_some(Origin {
            mode: self.mode,
            anchor: self.anchor,
            caret: self.doc.cursor,
            select: self.g.select || self.select_resume,
        })
    }

    /// Present the children of `path` for this editor's mode using the shared `register`.
    pub fn menu_entries(&self, register: &Register, path: &[String]) -> Vec<MenuEntry> {
        register
            .input_maps
            .menus(|menu| menu.entries(path, self.menu_mode()))
    }

    /// Activate `path` from the native popup with the current selection and `resolve_screen`.
    pub fn activate_menu(
        &mut self,
        register: &mut Register,
        path: &[String],
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.invoke_menu(path, self.menu_mode(), true, register, resolve_screen)
    }

    /// Dispatch Ex `text` as a menu command, retaining a captured Visual `origin` when present.
    pub(super) fn menu_command(
        &mut self,
        text: &str,
        origin: Option<Origin>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Option<Action> {
        let text = text.trim_start();
        let (word, args) = split_word(text);
        let command = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
        if let Some(definition) = parse_command(command) {
            let priority = word[..word.len() - command.len()].parse::<u32>().ok();
            return Some(self.configure_menu(definition, priority, args, register));
        }
        if let Some(remove) = tip_command(word) {
            return Some(self.configure_menu_tip(remove, args, register));
        }
        let (range, text) = self.ex_range(text)?;
        let (verb, mut args) = split_word(text);
        let popup = is_popup(verb);
        if !popup && !is_execute(verb) {
            return None;
        }
        let mut mode = None;
        if !popup {
            let (name, rest) = split_word(args);
            if args.len() > name.len()
                && let Some(explicit) = MenuMode::from_name(name)
            {
                mode = Some(explicit);
                args = rest;
            }
        }
        let path = if popup && args.is_empty() {
            Vec::new()
        } else {
            match parse_path(args) {
                Ok((labels, rest)) if rest.is_empty() => names(labels),
                _ => {
                    self.message = i18n::text("vim_menu.invalid_path").into();
                    return Some(Action::None);
                }
            }
        };
        let prior = self.menu_origin();
        if let Some(ref range) = range
            && (popup || mode.is_none())
        {
            let matches_range = |anchor: usize, caret: usize| {
                self.doc.location(anchor.min(caret)).0 == range.start
                    && self.doc.location(anchor.max(caret)).0 + 1 == range.end
            };
            let current = origin.filter(|origin| matches_range(origin.anchor, origin.caret));
            let saved = self.g.saved_visual().and_then(|(mode, anchor, caret)| {
                matches_range(anchor, caret).then_some(Origin {
                    mode,
                    anchor,
                    caret,
                    select: false,
                })
            });
            let mut restored = current.or(saved).unwrap_or(Origin {
                mode: Mode::VisualLine,
                anchor: self.doc.starts[range.start],
                caret: self.doc.starts[range.end - 1],
                select: false,
            });
            restored.select &= popup;
            self.restore_menu_origin(restored);
        } else if popup && let Some(origin) = origin {
            self.restore_menu_origin(origin);
        }
        if popup {
            let has_children = register.input_maps.menus(|menu| {
                menu.children(&path)
                    .is_some_and(|children| !children.is_empty())
            });
            if !has_children {
                if let Some(prior) = prior {
                    self.restore_menu_origin(prior);
                }
                self.message = i18n::text("vim_menu.no_entries").into();
                return Some(Action::None);
            }
            return Some(Action::VimMenu { path });
        }
        let mode = mode.unwrap_or_else(|| match self.menu_mode() {
            MenuMode::Select if range.is_none() => MenuMode::Normal,
            mode => mode,
        });
        Some(self.invoke_menu(&path, mode, false, register, resolve_screen))
    }

    /// Apply or list help text independently of the existing menu action.
    fn configure_menu_tip(&mut self, remove: bool, args: &str, register: &Register) -> Action {
        let (labels, rhs) = if args.is_empty() {
            (Vec::new(), "")
        } else {
            match parse_path(args) {
                Ok(parsed) => parsed,
                Err(error) => {
                    self.ex_failed = true;
                    self.message = i18n::text(error).into();
                    return Action::None;
                }
            }
        };
        let path = names(labels);
        if !remove && rhs.is_empty() {
            let listing = register.input_maps.menus(|menu| menu.tip_listing(&path));
            return if listing.is_empty() {
                self.message = i18n::text("vim_menu.no_entries").into();
                Action::None
            } else {
                Action::ShowListing(listing)
            };
        }
        if path.is_empty() || remove && !rhs.is_empty() {
            self.ex_failed = true;
            self.message = i18n::text("vim_menu.invalid_path").into();
        } else if !register
            .input_maps
            .change_menus(|menu| menu.change_tip(&path, (!remove).then_some(rhs)))
        {
            self.ex_failed = true;
            self.message = i18n::text("vim_menu.not_found").into();
        }
        Action::None
    }

    /// Write or list the menu `definition` described by `args`, honoring the given `priority`.
    fn configure_menu(
        &mut self,
        definition: MenuCommand,
        priority: Option<u32>,
        mut args: &str,
        register: &Register,
    ) -> Action {
        let (first, rest) = split_word(args);
        let change = match first {
            "enable" => Some(true),
            "disable" => Some(false),
            _ => None,
        };
        if change.is_some() {
            args = rest;
        }
        let mut priorities = vec![priority.unwrap_or(500)];
        if matches!(definition.operation, Operation::Define { .. }) && change.is_none() {
            loop {
                let (word, rest) = split_word(args);
                if matches!(word, "<silent>" | "<special>" | "<script>") {
                    args = rest;
                } else if let Some(priority) = parse_priorities(word) {
                    priorities = priority;
                    args = rest;
                } else {
                    break;
                }
            }
        }
        let (mut path, rhs) = if args.is_empty() {
            (Vec::new(), "")
        } else {
            match parse_path(args) {
                Ok(result) => result,
                Err(error) => {
                    self.message = i18n::text(error).into();
                    return Action::None;
                }
            }
        };
        let wildcard = rhs.is_empty() && path.last().is_some_and(|label| label.name == "*");
        if wildcard {
            path.pop();
        }
        let modes = definition.modes;
        match definition.operation {
            Operation::Remove | Operation::Define { .. }
                if change.is_some() || matches!(definition.operation, Operation::Remove) =>
            {
                if wildcard || !path.is_empty() && rhs.is_empty() {
                    let path = names(path);
                    if !register
                        .input_maps
                        .change_menus(|menu| menu.edit(&path, modes, change))
                    {
                        self.message = i18n::text("vim_menu.not_found").into();
                    }
                } else {
                    self.message = i18n::text("vim_menu.invalid_path").into();
                }
            }
            Operation::Define { .. } if rhs.is_empty() => {
                let listing = register
                    .input_maps
                    .menus(|menu| menu.listing(&names(path), modes));
                self.message = if listing.is_empty() {
                    i18n::text("vim_menu.no_entries").into()
                } else {
                    listing
                };
            }
            Operation::Define { recursive } => {
                let bindings = modes.iter().map(|&mode| {
                    let direct = rhs
                        .as_bytes()
                        .get(..5)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"<cmd>"));
                    let replacement = if definition.all_modes && !direct {
                        match mode {
                            MenuMode::Insert => format!("<C-\\><C-o>{rhs}"),
                            MenuMode::Visual
                            | MenuMode::Select
                            | MenuMode::Operator
                            | MenuMode::Command => {
                                format!("<C-c>{rhs}<C-\\><C-g>")
                            }
                            _ => rhs.into(),
                        }
                    } else {
                        rhs.into()
                    };
                    (
                        mode,
                        Binding {
                            replacement,
                            recursive,
                            restore_select: modes.contains(&MenuMode::Visual),
                            enabled: true,
                        },
                    )
                });
                if let Err(error) = register
                    .input_maps
                    .change_menus(|menu| menu.define(path, &priorities, bindings))
                {
                    self.message = i18n::text(error).into();
                }
            }
            Operation::Remove => unreachable!("remove commands handled above"),
        }
        Action::None
    }

    /// Restore a checked `origin` after the command line accepted a menu command.
    fn restore_menu_origin(&mut self, origin: Origin) {
        self.mode = origin.mode;
        self.anchor = self
            .doc
            .text
            .floor_char_boundary(origin.anchor.min(self.doc.text.len()));
        self.set_cursor(origin.caret);
        self.g.select = origin.select;
    }

    /// Look up the requested `path` for `mode` and execute its existing mapping.
    fn invoke_menu(
        &mut self,
        path: &[String],
        mode: MenuMode,
        from_popup: bool,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let binding = register.input_maps.menus(|menu| {
            let node = menu.node(path).ok_or("vim_menu.not_found")?;
            if !node.children.is_empty() || node.label.is_separator() {
                return Err("vim_menu.not_leaf");
            }
            node.bindings[mode as usize]
                .clone()
                .filter(|binding| binding.enabled)
                .ok_or("vim_menu.unavailable")
        });
        match binding {
            Ok(binding) if mode != MenuMode::Terminal => {
                let action = self.flush_input_prefix(register, resolve_screen);
                if action != Action::None {
                    return action;
                }
                self.run_menu_mapping(
                    binding.replacement,
                    binding.recursive,
                    binding.restore_select && from_popup,
                    register,
                    resolve_screen,
                )
            }
            Ok(_) => {
                self.message = i18n::text("vim_menu.unavailable").into();
                Action::None
            }
            Err(error) => {
                self.message = i18n::text(error).into();
                Action::None
            }
        }
    }

    /// Determine which menu definition applies to this input mode.
    fn menu_mode(&self) -> MenuMode {
        if self.mode.is_visual() {
            if self.g.select {
                MenuMode::Select
            } else {
                MenuMode::Visual
            }
        } else if self.mode.is_insert() {
            MenuMode::Insert
        } else if self.mode.accepts_text() {
            MenuMode::Command
        } else if matches!(self.pending, Some(Pending::Operator(_))) {
            MenuMode::Operator
        } else {
            MenuMode::Normal
        }
    }
}

/// Recognize Ex execution spellings for menu leaves.
fn is_execute(word: &str) -> bool {
    word.len() >= 2 && "emenu".starts_with(word)
}

/// Recognize Ex popup spellings for menu branches.
fn is_popup(word: &str) -> bool {
    let word = word.strip_suffix('!').unwrap_or(word);
    word.len() >= 4 && "popup".starts_with(word)
}

/// Split the first word from the remaining arguments.
fn split_word(text: &str) -> (&str, &str) {
    text.split_once(char::is_whitespace)
        .map_or((text, ""), |(word, rest)| (word, rest.trim_start()))
}

/// Decode explicit per-level priorities from a menu argument `word`.
fn parse_priorities(word: &str) -> Option<Vec<u32>> {
    if word.is_empty() || !word.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    word.split('.')
        .map(|part| {
            if part.is_empty() {
                Some(500)
            } else {
                part.parse().ok().map(|n| if n == 0 { 500 } else { n })
            }
        })
        .collect()
}

/// Parse one escaped menu path from `args`, leaving its remaining command text.
fn parse_path(args: &str) -> Result<(Vec<Label>, &str), &'static str> {
    let mut path = Vec::new();
    let mut part = String::new();
    let mut input = args.char_indices();
    let mut rest = "";
    while let Some((index, ch)) = input.next() {
        match ch {
            '\\' | '\u{16}' => {
                let Some((_, escaped)) = input.next() else {
                    return Err("vim_menu.invalid_path");
                };
                part.push(escaped);
            }
            '.' => path.push(Label::parse(&std::mem::take(&mut part))),
            ch if ch.is_whitespace() => {
                rest = args[index..].trim_start();
                break;
            }
            ch => part.push(ch),
        }
    }
    path.push(Label::parse(&part));
    if path.len() > 32 || path.iter().any(|part| part.name.is_empty()) {
        return Err("vim_menu.invalid_path");
    }
    Ok((path, rest))
}

/// Extract canonical names from `labels` for path lookup.
fn names(labels: Vec<Label>) -> Vec<String> {
    labels.into_iter().map(|label| label.name).collect()
}

/// Escape characters that carry Vim menu meaning when completing `name` into Ex input.
fn escape_name(name: &str) -> String {
    let mut escaped = String::new();
    for ch in name.chars() {
        if ch == '&' {
            escaped.push('&');
        } else if ch.is_whitespace() || matches!(ch, '.' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Submit an Ex `command` from this editor's current mode.
    fn ex(vim: &mut Vim, register: &mut Register, command: &str) -> Action {
        vim.key(":", register);
        vim.paste_text(command, register);
        vim.key("Enter", register)
    }

    /// Invoke one native PopUp `label` in the model's active mode.
    fn activate(vim: &mut Vim, register: &mut Register, label: &str) {
        vim.activate_menu(
            register,
            &["PopUp".into(), label.into()],
            &mut |_, _, _, _| None,
        );
    }

    /// Shared menu bindings restore Select unless they use gV or delete the selected area.
    #[test]
    fn native_menu_replay_distinguishes_select_visual_and_gv() {
        let mut vim = Vim::new(Document::new("first\nsecond\nthird\n"));
        let mut register = Register::default();
        for definition in [
            "vnoremenu PopUp.Restore <Esc>",
            "vnoremenu PopUp.Exit <Esc>gV",
            "vnoremenu PopUp.Delete d",
            "xnoremenu PopUp.Visual <Esc>",
            "snoremenu PopUp.Select <Esc>",
        ] {
            assert_eq!(ex(&mut vim, &mut register, definition), Action::None);
        }
        vim.text("gH", &mut register);
        vim.key("Down", &mut register);
        let selected = vim.selection();
        assert!(vim.g.select && selected.is_some());
        let entries = vim.menu_entries(&register, &["PopUp".into()]);
        assert!(
            entries
                .iter()
                .any(|entry| entry.label == "Visual" && !entry.enabled)
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.label == "Select" && entry.enabled)
        );
        activate(&mut vim, &mut register, "Restore");
        assert!(vim.g.select);
        assert_eq!(vim.selection(), selected);
        activate(&mut vim, &mut register, "Exit");
        assert_eq!(vim.mode, Mode::Normal);
        assert_eq!(vim.selection(), None);
        vim.text("gv", &mut register);
        assert_eq!(vim.selection(), selected);
        activate(&mut vim, &mut register, "Restore");
        assert_eq!(vim.mode, Mode::Normal);
        vim.text("gggH", &mut register);
        activate(&mut vim, &mut register, "Delete");
        assert_eq!(vim.mode, Mode::Normal);
        assert_eq!(vim.doc.text, "second\nthird\n");
    }

    /// Ex paths retain the true Visual shape and a direct <Cmd> RHS keeps the active mode.
    #[test]
    fn popup_ex_retains_visual_area_and_emenu_honors_explicit_addresses() {
        let mut vim = Vim::new(Document::new("first\nsecond\nthird\n"));
        let mut register = Register::default();
        ex(
            &mut vim,
            &mut register,
            "xnoremenu Test.Echo <Cmd>echo 'active'<CR>",
        );
        ex(&mut vim, &mut register, "xnoremenu Test.Delete d");
        vim.text("vll", &mut register);
        let selected = vim.selection();
        assert_eq!(
            ex(&mut vim, &mut register, "popup Test"),
            Action::VimMenu {
                path: vec!["Test".into()]
            }
        );
        assert_eq!(vim.selection(), selected);
        vim.activate_menu(
            &mut register,
            &["Test".into(), "Echo".into()],
            &mut |_, _, _, _| None,
        );
        assert_eq!(vim.selection(), selected);
        assert_eq!(vim.message, "active");
        vim.key("Escape", &mut register);
        ex(&mut vim, &mut register, "2,3emenu Test.Delete");
        assert_eq!(vim.doc.text, "first\n");
    }

    /// Menu mutations preserve sibling ordering, distinguish modes, and expose escaped Ex completion.
    #[test]
    fn menu_paths_priorities_mode_removal_and_completion_use_shared_tree() {
        let mut builder = Vim::new(Document::new(""));
        let mut register = Register::default();
        ex(
            &mut builder,
            &mut register,
            "20menu &Edit.&Rename\\ file<Tab>r <Cmd>echo 'rename'<CR>",
        );
        ex(
            &mut builder,
            &mut register,
            "10menu &File.Open\\.Document <Nop>",
        );
        ex(&mut builder, &mut register, "menu Edit.-Between- :");
        let mut vim = Vim::new(Document::new(""));
        let root = vim.menu_entries(&register, &[]);
        assert_eq!(
            root.iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["File", "Edit"]
        );
        let children = vim.menu_entries(&register, &["Edit".into()]);
        assert_eq!(children[0].label, "Rename file");
        assert_eq!(children[0].mnemonic, Some('R'));
        assert_eq!(children[0].accelerator.as_deref(), Some("r"));
        assert!(children[1].separator && !children[1].enabled);
        ex(&mut vim, &mut register, "nmenu disable Edit.*");
        assert!(!vim.menu_entries(&register, &["Edit".into()])[0].enabled);
        ex(&mut vim, &mut register, "nmenu enable &Edit.&Rename\\ file");
        ex(&mut vim, &mut register, "emenu n Edit.Rename\\ file");
        assert_eq!(vim.message, "rename");
        vim.key(":", &mut register);
        vim.paste_text("emenu File.Open", &mut register);
        vim.key("Tab", &mut register);
        assert_eq!(vim.command, "emenu File.Open\\.Document");
        vim.key("Escape", &mut register);
        ex(&mut vim, &mut register, "nunmenu Edit");
        assert!(!vim.menu_entries(&register, &["Edit".into()])[0].enabled);
        ex(&mut vim, &mut register, "aunmenu *");
        assert!(vim.menu_entries(&register, &[]).is_empty());
    }

    /// Menu tips use the popup's real entries and can be removed independently of their actions.
    #[test]
    fn menu_tips_share_the_popup_tree_without_changing_actions() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        let menu = ["Edit".into()];
        ex(
            &mut vim,
            &mut register,
            "noremenu &Edit.Rename\\ file <Cmd>echo 'renamed'<CR>",
        );
        ex(&mut vim, &mut register, "menu Edit.Delete <Nop>");
        ex(
            &mut vim,
            &mut register,
            "tmenu Edit.Rename\\ file Rename the current entry",
        );
        ex(&mut vim, &mut register, "tm Edit.Delete Remove this entry");
        let entries = vim.menu_entries(&register, &menu);
        assert_eq!(
            entries[0].tooltip.as_deref(),
            Some("Rename the current entry")
        );
        assert_eq!(entries[1].tooltip.as_deref(), Some("Remove this entry"));
        let Action::ShowListing(listing) = ex(&mut vim, &mut register, "tmenu Edit") else {
            panic!("expected a menu tip listing");
        };
        assert_eq!(
            listing,
            "Edit.Rename file  Rename the current entry\nEdit.Delete  Remove this entry"
        );
        ex(&mut vim, &mut register, "tunmenu Edit.*");
        let entries = vim.menu_entries(&register, &menu);
        assert!(
            entries
                .iter()
                .all(|entry| entry.enabled && entry.tooltip.is_none())
        );
        ex(&mut vim, &mut register, "emenu Edit.Rename\\ file");
        assert_eq!(vim.message, "renamed");
    }
}
