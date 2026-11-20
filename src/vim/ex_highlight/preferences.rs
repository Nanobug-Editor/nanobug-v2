//! Persist settings-page GUI attributes while retaining Ex links and palette-relative colors.

use super::{HighlightAttributes, parse_attributes};
use crate::{app_data::settings, vim::InputMappings};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, path::Path};

/// Keep the source spelling for Ex listings and precompute the attributes used by every frame.
pub(super) struct GuiPreference {
    pub(super) spelling: String,
    pub(super) attributes: HighlightAttributes,
}

impl HighlightAttributes {
    /// Canonical name of the single underline shape supported by this style.
    pub fn underline_shape(&self) -> &'static str {
        [
            ("underline", self.underline),
            ("undercurl", self.undercurl),
            ("underdouble", self.underdouble),
            ("underdotted", self.underdotted),
            ("underdashed", self.underdashed),
        ]
        .into_iter()
        .find(|(_, active)| *active)
        .map_or("none", |(name, _)| name)
    }

    /// Names of the independently composable font decorations in display order.
    pub fn decorations(&self) -> Vec<&'static str> {
        [
            ("bold", self.bold),
            ("italic", self.italic),
            ("strikethrough", self.strikethrough),
            ("overline", self.overline),
        ]
        .into_iter()
        .filter_map(|(name, active)| active.then_some(name))
        .collect()
    }

    /// Keep all GUI flags, including attributes that the form does not expose.
    fn gui_names(&self) -> String {
        let mut names = self.decorations();
        if self.underline_shape() != "none" {
            names.push(self.underline_shape());
        }
        for (name, enabled) in [
            ("reverse", self.reverse),
            ("standout", self.standout),
            ("dim", self.dim),
            ("conceal", self.conceal),
            ("nocombine", self.nocombine),
        ] {
            if enabled {
                names.push(name);
            }
        }
        if names.is_empty() {
            "none".into()
        } else {
            names.join(",")
        }
    }
}

impl InputMappings {
    /// Install validated portable GUI overrides after the host palette and Vim maps are available.
    pub fn load_highlight_preferences(&self, path: &Path) -> Result<()> {
        let document = settings::read(path)?;
        let mut values = BTreeMap::new();
        if let Some(section) = document.get("highlight") {
            for (group, gui) in section
                .as_table_like()
                .context("Expected [highlight] table")?
                .iter()
            {
                let value = gui.as_str().context("Expected a GUI attribute list")?;
                let attributes = parse_attributes(value).map_err(anyhow::Error::msg)?;
                values.insert(
                    group.to_ascii_lowercase(),
                    GuiPreference {
                        spelling: value.into(),
                        attributes,
                    },
                );
            }
        }
        let mut colors = BTreeMap::new();
        if let Some(section) = document.get("highlight_colors") {
            for (group, value) in section
                .as_table_like()
                .context("Expected [highlight_colors] table")?
                .iter()
            {
                let hex = value
                    .as_str()
                    .and_then(|s| s.strip_prefix('#'))
                    .filter(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    .context("Expected #RRGGBB")?;
                colors.insert(group.to_ascii_lowercase(), u32::from_str_radix(hex, 16)?);
            }
        }
        self.edit_highlights(|highlights| {
            highlights.preferences = values;
            highlights.foregrounds = colors;
        });
        Ok(())
    }

    /// Effective style after releasing just this page's saved settings. Ex links and parent preferences still apply.
    pub fn inherited_highlight_style(&self, group: &str) -> Option<super::HighlightStyle> {
        let key = group.to_ascii_lowercase();
        self.edit_highlights(|h| {
            let color = h.foregrounds.remove(&key);
            let attrs = h.preferences.remove(&key);
            let style = h.style(group, false);
            if let Some(color) = color {
                h.foregrounds.insert(key.clone(), color);
            }
            if let Some(attrs) = attrs {
                h.preferences.insert(key, attrs);
            }
            style
        })
    }

    /// Explicit settings-page foreground, with None meaning use the theme and inherited styles.
    pub fn highlight_foreground(&self, group: &str) -> Option<u32> {
        self.edit_highlights(|h| h.foregrounds.get(&group.to_ascii_lowercase()).copied())
    }

    /// Commit one group's GUI attributes, releasing the override when it matches its inherited style.
    pub fn save_highlight_preference(
        &self,
        group: &str,
        attributes: HighlightAttributes,
        path: Option<&Path>,
        restore: Option<settings::RestoreFields<'_>>,
    ) -> Result<()> {
        self.save_highlight_appearance(
            group,
            attributes,
            self.highlight_foreground(group),
            path,
            restore,
        )
    }

    /// Persist both controls as one edit and update the shared registry only after the write succeeds.
    pub fn save_highlight_appearance(
        &self,
        group: &str,
        attributes: HighlightAttributes,
        foreground: Option<u32>,
        path: Option<&Path>,
        restore: Option<settings::RestoreFields<'_>>,
    ) -> Result<()> {
        let key = group.to_ascii_lowercase();
        self.edit_highlights(|highlights| {
            let old = highlights.preferences.remove(&key);
            let inherited = highlights
                .style(group, false)
                .map(|style| style.attributes)
                .unwrap_or_default();
            let opening = restore
                .filter(|r| r.names.contains(&key.as_str()))
                .and_then(|r| r.snapshot.string(&["highlight", &key]))
                .filter(|value| parse_attributes(value).ok() == Some(attributes));
            let next = opening
                .map(str::to_owned)
                .or_else(|| (attributes != inherited).then(|| attributes.gui_names()));
            if let Some(old) = old {
                highlights.preferences.insert(key.clone(), old);
            }
            if let Some(path) = path {
                settings::save_fields(
                    path,
                    &[
                        (&["highlight", &key], next.as_deref().map(Into::into)),
                        (
                            &["highlight_colors", &key],
                            foreground.map(|rgb| format!("#{rgb:06x}").into()),
                        ),
                    ],
                    restore,
                )?;
            }
            if let Some(rgb) = foreground {
                highlights.foregrounds.insert(key.clone(), rgb);
            } else {
                highlights.foregrounds.remove(&key);
            }
            if let Some(value) = next {
                highlights.preferences.insert(
                    key,
                    GuiPreference {
                        spelling: value,
                        attributes,
                    },
                );
            } else {
                highlights.preferences.remove(&key);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::HighlightStyle;

    /// A saved child override may have the same appearance as its parent when the settings open.
    #[test]
    fn cancel_keeps_an_explicit_child_when_parent_later_changes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "[highlight]\nfunction = 'bold'\nmethod = 'bold'\n")?;
        let snapshot = settings::SettingsSnapshot::load(Some(&path))?;
        let maps = InputMappings::default();
        maps.set_highlight_palette([
            ("Method", HighlightStyle::default()),
            ("Function", HighlightStyle::default()),
        ]);
        maps.set_highlight_inheritance(vec![("Method", "Function")]);
        maps.load_highlight_preferences(&path)?;
        let bold = maps.highlight_style("Method").unwrap().attributes;
        maps.save_highlight_preference("Method", Default::default(), Some(&path), None)?;
        maps.save_highlight_preference(
            "Method",
            bold,
            Some(&path),
            Some(settings::RestoreFields {
                snapshot: &snapshot,
                names: &["method"],
            }),
        )?;
        maps.save_highlight_preference("Function", Default::default(), Some(&path), None)?;
        assert!(maps.highlight_style("Method").unwrap().attributes.bold);
        Ok(())
    }

    /// Save real attributes, retain theme colors across reload and release an opening-time inherited style.
    #[test]
    fn gui_choices_reload_and_release_without_flattening_palette() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        let opening = "# keep this\n[vim.options]\nignorecase = true\n";
        std::fs::write(&path, opening)?;
        let snapshot = settings::SettingsSnapshot::load(Some(&path))?;
        let maps = InputMappings::default();
        let style = HighlightStyle {
            foreground: Some(0x123456),
            ..Default::default()
        };
        maps.set_highlight_palette([("Comment", style)]);
        maps.load_config(&path)?;
        assert_eq!(maps.highlight_style("Comment"), Some(style));
        let custom = HighlightAttributes {
            bold: true,
            italic: true,
            undercurl: true,
            ..Default::default()
        };
        maps.save_highlight_preference("Comment", custom, Some(&path), None)?;
        let reloaded = InputMappings::default();
        reloaded.set_highlight_palette([("Comment", style)]);
        reloaded.load_highlight_preferences(&path)?;
        assert_eq!(
            reloaded.highlight_override("Comment"),
            Some(HighlightStyle {
                attributes: custom,
                ..style
            })
        );
        maps.save_highlight_preference(
            "Comment",
            style.attributes,
            Some(&path),
            Some(settings::RestoreFields {
                snapshot: &snapshot,
                names: &["comment"],
            }),
        )?;
        assert_eq!(maps.highlight_override("Comment"), None);
        assert_eq!(std::fs::read_to_string(path)?, opening);
        Ok(())
    }
}
