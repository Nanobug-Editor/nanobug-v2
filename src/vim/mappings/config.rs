//! Persist workspace-level Vim definitions and option defaults.

use super::{
    Definitions, InputMappings, KEY_LIMIT, LoadedKeymap, MappingTables, ModeGroup, Rule,
    RuleSource, VimOptions, abbreviation_kind, display_mapping_keys, keymap_error, mapping_lhs,
};
use crate::app_data::settings;
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Concrete mapping tables to round trip in a stable order.
const MODES: &[ModeGroup] = &[
    ModeGroup::Language,
    ModeGroup::Normal,
    ModeGroup::Insert,
    ModeGroup::Command,
    ModeGroup::Terminal,
    ModeGroup::Operator,
    ModeGroup::Visual,
    ModeGroup::Select,
    ModeGroup::InsertAbbrev,
    ModeGroup::CommandAbbrev,
];

/// Portable settings stored under `[vim]` after an explicit save.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct VimConfig {
    /// Default Vim option values for new editors.
    options: VimOptions,
    /// Selected global keymap file, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    keymap: Option<SavedKeymap>,
    /// Concrete global mapping or abbreviation definitions.
    mappings: Vec<SavedMapping>,
}

/// The selected keymap and the directory where it was configured.
#[derive(Deserialize, Serialize)]
struct SavedKeymap {
    /// Catalog name used by `:set keymap`.
    name: String,
    /// Project root used before looking in the portable catalog.
    directory: PathBuf,
}

/// One installed rule, including its leader and precedence when defined.
#[derive(Deserialize, Serialize)]
struct SavedMapping {
    /// Concrete table containing this definition.
    mode: ModeGroup,
    /// User-entered key or word notation.
    lhs: String,
    /// User-entered key sequence or expression on the right.
    rhs: String,
    /// Captured leader used for the two sides of this mapping.
    leader: String,
    /// Allow mappings when expanding the right side.
    recursive: bool,
    /// Interpret the right side as an input expression.
    expression: bool,
    /// Restore Select mode after a shared Visual/Select map.
    restore_select: bool,
    /// Resolve a matching prefix immediately when it has priority.
    nowait: bool,
    /// Definition ordinal used when resolving overlapping prefixes.
    order: u64,
}

impl InputMappings {
    /// Export sourceable global settings and mappings without any buffer-local definitions.
    pub fn ex_configuration_lines(&self) -> Vec<String> {
        let definitions = self.0.read().unwrap();
        let mut lines = definitions.options.ex_export_lines();
        lines.extend(ex_mapping_lines(
            &definitions.tables,
            &definitions.options.mapleader,
            false,
        ));
        if !definitions.keymap.name.is_empty() {
            lines.push(format!("set keymap={}", definitions.keymap.name));
        }
        lines
    }

    /// Load definitions from `path` before editors attach to this workspace's mappings.
    pub fn load_config(&self, path: &Path) -> Result<()> {
        let Some(config) = settings::read_vim::<VimConfig>(path)? else {
            return Ok(());
        };
        ensure!(config.options.validate(), "Invalid Vim option value");
        let mut definitions = Definitions {
            options: config.options,
            ..Definitions::default()
        };
        for saved in config.mappings {
            saved.install(&mut definitions.tables)?;
        }
        let mut keymap_error_message = None;
        if let Some(keymap) = config.keymap {
            match LoadedKeymap::load(&keymap.directory, &keymap.name) {
                Ok(keymap) => definitions.keymap = keymap,
                Err(error) => keymap_error_message = Some(keymap_error(&keymap.name, error)),
            }
        }
        *self.0.write().unwrap() = definitions;
        if let Some(error) = keymap_error_message {
            return Err(anyhow!(error));
        }
        Ok(())
    }

    /// Atomically write the current global definitions to `path` without buffer-local state.
    pub fn save_config(&self, path: &Path) -> Result<()> {
        let config = {
            let definitions = self.0.read().unwrap();
            let mut mappings = Vec::new();
            for &mode in MODES {
                mappings.extend(
                    definitions
                        .tables
                        .table(mode)
                        .values()
                        .filter_map(|rule| SavedMapping::from_rule(mode, rule)),
                );
            }
            let keymap = &definitions.keymap;
            VimConfig {
                options: definitions.options.clone(),
                keymap: (!keymap.name.is_empty()).then(|| SavedKeymap {
                    name: keymap.name.clone(),
                    directory: keymap.directory.clone(),
                }),
                mappings,
            }
        };
        settings::save_vim(path, &config)
    }
}

impl super::Vim {
    /// Export mapping definitions that belong to this buffer rather than the workspace.
    pub fn ex_local_mapping_lines(&self) -> Vec<String> {
        let leader = self.options.options().mapleader;
        ex_mapping_lines(&self.input_maps.local.read().unwrap(), &leader, true)
    }
}

/// Write the supported Ex spelling of each rule in dependency and precedence order.
fn ex_mapping_lines(tables: &MappingTables, leader: &str, local: bool) -> Vec<String> {
    use ModeGroup::*;
    let mut definitions = Vec::new();
    for &mode in MODES {
        definitions.extend(tables.table(mode).iter().filter_map(|(keys, rule)| {
            rule.source
                .as_ref()
                .map(|source| (mode, keys, rule, source))
        }));
    }
    definitions.sort_by_key(|&(_, _, rule, _)| rule.order);
    let mut lines = Vec::new();
    let mut active_leader = leader;
    if !local {
        lines.push(leader_command(leader));
    }
    for (mode, keys, rule, source) in definitions {
        if source.leader != active_leader {
            lines.push(leader_command(&source.leader));
            active_leader = &source.leader;
        }
        let (prefix, suffix) = match mode {
            Language => ("l", "map"),
            Normal => ("n", "map"),
            Insert => ("i", "map"),
            Command => ("c", "map"),
            Terminal => ("t", "map"),
            Operator => ("o", "map"),
            Visual => ("x", "map"),
            Select => ("s", "map"),
            InsertAbbrev => ("i", "abbrev"),
            CommandAbbrev => ("c", "abbrev"),
            _ => unreachable!("concrete map"),
        };
        let lhs = if mode.abbreviations() {
            source.lhs.clone()
        } else {
            display_mapping_keys(keys).replace('|', "<Bar>")
        };
        let rhs = if rule.expression {
            rule.replacement.clone()
        } else {
            rule.replacement.replace('|', "<Bar>")
        };
        let recurse = if rule.recursive { "" } else { "nore" };
        let scope = if local { "<buffer> " } else { "" };
        let expression = if rule.expression { "<expr> " } else { "" };
        let wait = if rule.nowait { "<nowait> " } else { "" };
        lines.push(format!(
            "{prefix}{recurse}{suffix} {scope}{expression}{wait}{lhs} {rhs}"
        ));
    }
    if active_leader != leader {
        lines.push(leader_command(leader));
    }
    lines
}

/// Quote a leader string in native Vim expression syntax.
fn leader_command(value: &str) -> String {
    format!("let g:mapleader = '{}'", value.replace('\'', "''"))
}

impl SavedMapping {
    /// Capture the user definition in `rule` from a concrete `mode` table.
    fn from_rule(mode: ModeGroup, rule: &Rule) -> Option<Self> {
        let source = rule.source.as_ref()?;
        Some(Self {
            mode,
            lhs: source.lhs.clone(),
            rhs: rule.replacement.clone(),
            leader: source.leader.clone(),
            recursive: rule.recursive,
            expression: rule.expression,
            restore_select: rule.restore_select,
            nowait: rule.nowait,
            order: rule.order,
        })
    }

    /// Restore this mapping into `definitions`, preserving relative precedence.
    fn install(self, definitions: &mut MappingTables) -> Result<()> {
        let keys = mapping_lhs(self.mode, &self.lhs, &self.leader);
        ensure!(
            MODES.contains(&self.mode)
                && !keys.is_empty()
                && self.lhs.len() <= KEY_LIMIT
                && !self.rhs.is_empty()
                && self.rhs.len() <= KEY_LIMIT * 8
                && (!self.mode.abbreviations() || abbreviation_kind(&self.lhs).is_some()),
            "Invalid saved Vim mapping: {}",
            self.lhs
        );
        definitions.order = definitions.order.max(self.order);
        definitions.table_mut(self.mode).insert(
            keys,
            Rule {
                replacement: self.rhs,
                source: Some(RuleSource {
                    lhs: self.lhs,
                    leader: self.leader,
                }),
                recursive: self.recursive,
                expression: self.expression,
                restore_select: self.restore_select,
                nowait: self.nowait,
                order: self.order,
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::Document,
        vim::{Register, Vim},
    };

    /// Enter an Ex command through the same input path used by an editor.
    fn ex(vim: &mut Vim, register: &mut Register, command: &str) {
        vim.key(":", register);
        vim.paste_text(command, register);
        vim.key("Enter", register);
    }

    /// Reload global mappings and captured leaders while leaving local rules in their original buffer.
    #[test]
    fn vim_config_reloads_global_definitions_and_preserves_adjacent_settings() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.toml");
        std::fs::write(&path, "# user preferences\n[ui]\nfont_size = 16\n")?;
        let maps = InputMappings::default();
        let mut register = Register::default();
        register.set_input_mappings(maps.clone());
        let mut vim = Vim::new(Document::new("one two"));
        ex(&mut vim, &mut register, "nnoremap <Leader>j w");
        ex(&mut vim, &mut register, "nnoremap <buffer> <F7> w");
        ex(&mut vim, &mut register, "let g:mapleader = ',,'");
        ex(&mut vim, &mut register, "nnoremap <Leader>j 0");
        maps.save_config(&path)?;

        let restored = InputMappings::default();
        restored.load_config(&path)?;
        assert_eq!(restored.options().mapleader, ",,");
        assert!(std::fs::read_to_string(&path)?.contains("[ui]\nfont_size = 16"));
        let mut register = Register::default();
        register.set_input_mappings(restored.clone());
        let mut vim = Vim::new(Document::new("one two"));
        assert!(!vim.accepts_input_mapping("F7", &register));
        vim.key(" ", &mut register);
        vim.key("j", &mut register);
        assert_eq!(vim.doc.cursor, 4);
        vim.key(",", &mut register);
        vim.key(",", &mut register);
        vim.key("j", &mut register);
        assert_eq!(vim.doc.cursor, 0);
        ex(&mut vim, &mut register, "nmapclear");
        restored.save_config(&path)?;
        let empty = InputMappings::default();
        empty.load_config(&path)?;
        register.set_input_mappings(empty);
        assert!(!vim.accepts_input_mapping(",", &register));
        Ok(())
    }
}
