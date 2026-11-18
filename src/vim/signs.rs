//! Sign definitions shared across the workspace's Ex interpreters.

use std::{collections::BTreeMap, path::PathBuf};
use unicode_width::UnicodeWidthStr;

/// A named sign that can be placed on one or more buffer lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignDefinition {
    /// Original name, normalized when numeric.
    pub name: String,
    /// Stored bitmap filename.
    pub icon: Option<PathBuf>,
    /// One or two display cells printed in the sign column.
    pub text: Option<String>,
    /// Highlight group applied to the line.
    pub linehl: Option<String>,
    /// Highlight group applied to the sign text.
    pub texthl: Option<String>,
    /// Highlight group applied to its line number.
    pub numhl: Option<String>,
    /// Highlight group used for the sign when it is on the cursor line.
    pub culhl: Option<String>,
    /// Default priority of new placements.
    pub priority: i32,
}

/// Attributes specified by a single :sign define command.
/// Omitted style fields keep their previous value. An empty value clears a field.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SignDefinitionUpdate {
    /// Bitmap filename to store.
    pub icon: Option<PathBuf>,
    /// Printable text no wider than two display cells.
    pub text: Option<String>,
    /// Highlight group applied to the line.
    pub linehl: Option<String>,
    /// Highlight group applied to the sign text.
    pub texthl: Option<String>,
    /// Highlight group applied to its line number.
    pub numhl: Option<String>,
    /// Highlight group used for the sign when it is on the cursor line.
    pub culhl: Option<String>,
    /// Default placement priority, or 10 if omitted or set to -1.
    pub priority: Option<i32>,
}

/// Whether future commands can refer to this definition by name.
struct DefinitionSlot {
    definition: SignDefinition,
    available: bool,
}

/// Active definitions and retired styles retained for already placed signs.
#[derive(Default)]
pub(super) struct SignDefinitions(BTreeMap<String, DefinitionSlot>);

impl SignDefinitions {
    /// Define `name` or merge its supplied attributes. Validate before changing shared state.
    pub(super) fn define(
        &mut self,
        name: &str,
        update: SignDefinitionUpdate,
    ) -> Result<(), String> {
        let name = normalized_name(name).ok_or_else(|| format!("Invalid sign name: {name}"))?;
        if let Some(text) = &update.text
            && (text.chars().any(char::is_control)
                || text.width() > 2
                || !text.is_empty() && text.width() == 0)
        {
            return Err(format!("Invalid sign text: {text}"));
        }
        let mut definition = self.get(name, false).unwrap_or_else(|| SignDefinition {
            name: name.into(),
            icon: None,
            text: None,
            linehl: None,
            texthl: None,
            numhl: None,
            culhl: None,
            priority: 10,
        });
        if let Some(icon) = update.icon {
            definition.icon = (!icon.as_os_str().is_empty()).then_some(icon);
        }
        if let Some(text) = update.text {
            definition.text = (!text.is_empty()).then_some(text);
        }
        for (field, value) in [
            (&mut definition.linehl, update.linehl),
            (&mut definition.texthl, update.texthl),
            (&mut definition.numhl, update.numhl),
            (&mut definition.culhl, update.culhl),
        ] {
            if let Some(value) = value {
                *field = (!value.is_empty()).then_some(value);
            }
        }
        definition.priority = update.priority.filter(|&value| value != -1).unwrap_or(10);
        self.0.insert(
            name.into(),
            DefinitionSlot {
                definition,
                available: true,
            },
        );
        Ok(())
    }

    /// Get a copy of the style of a named sign. Include retired definitions only while drawing.
    pub(super) fn get(&self, name: &str, include_retired: bool) -> Option<SignDefinition> {
        let slot = self.0.get(normalized_name(name)?)?;
        (include_retired || slot.available).then(|| slot.definition.clone())
    }

    /// Snapshot available types for listing and command completion.
    pub(super) fn list(&self) -> Vec<SignDefinition> {
        self.0
            .values()
            .filter(|slot| slot.available)
            .map(|slot| slot.definition.clone())
            .collect()
    }

    /// Retire `name`, or all names when absent, for future placements.
    pub(super) fn undefine(&mut self, name: Option<&str>) -> bool {
        let Some(name) = name else {
            for slot in self.0.values_mut() {
                slot.available = false;
            }
            return true;
        };
        normalized_name(name)
            .and_then(|name| self.0.get_mut(name))
            .is_some_and(|slot| std::mem::replace(&mut slot.available, false))
    }
}

/// Reject malformed names and treat numeric spellings with leading zeros as one type.
fn normalized_name(name: &str) -> Option<&str> {
    if name.is_empty() || name.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return None;
    }
    if !name.as_bytes()[0].is_ascii_digit() {
        return Some(name);
    }
    if !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let stripped = name.trim_start_matches('0');
    Some(if stripped.is_empty() { "0" } else { stripped })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep text and highlights when updating a type, without accepting unprintable marks.
    #[test]
    fn sign_definitions_validate_text_and_merge_styles() -> Result<(), String> {
        let mut registry = SignDefinitions::default();
        registry.define(
            "002",
            SignDefinitionUpdate {
                text: Some("标".into()),
                priority: Some(17),
                texthl: Some("Search".into()),
                ..Default::default()
            },
        )?;
        assert_eq!(registry.get("02", false).unwrap().priority, 17);
        assert!(
            registry
                .define(
                    "2",
                    SignDefinitionUpdate {
                        text: Some("abc".into()),
                        ..Default::default()
                    }
                )
                .is_err()
        );
        assert!(
            registry
                .define(
                    "2",
                    SignDefinitionUpdate {
                        text: Some("\n".into()),
                        ..Default::default()
                    }
                )
                .is_err()
        );
        assert!(
            registry
                .define("2bad", SignDefinitionUpdate::default())
                .is_err()
        );
        registry.define(
            "2",
            SignDefinitionUpdate {
                linehl: Some("Comment".into()),
                ..Default::default()
            },
        )?;
        let sign = &registry.list()[0];
        assert_eq!(sign.name, "2");
        assert_eq!(sign.priority, 10);
        assert_eq!(sign.text.as_deref(), Some("标"));
        assert_eq!(sign.texthl.as_deref(), Some("Search"));
        assert_eq!(sign.linehl.as_deref(), Some("Comment"));
        assert!(registry.undefine(Some("0002")));
        assert!(!registry.undefine(Some("2")));
        assert!(registry.list().is_empty());
        assert!(registry.get("2", false).is_none());
        assert_eq!(registry.get("2", true).as_ref(), Some(sign));
        Ok(())
    }
}
