//! Parse appearance commands and store their values in the portable UI settings.

use super::{AppearanceSettings, GridFont, ThemeChoice};
use anyhow::Result;
use nanobug::app_data::settings;
use std::{ops::RangeInclusive, path::Path};
use toml_edit::{Item, Value};

/// Supported base sizes in logical pixels.
const FONT_SIZE_RANGE: RangeInclusive<f64> = 12. ..=24.;
/// Supported user zoom percentages, independent of display DPI.
const ZOOM_RANGE: RangeInclusive<f64> = 75. ..=150.;

/// One preference change, shared by the command and configuration paths.
#[derive(Clone, Copy)]
pub(crate) enum AppearanceChange {
    /// Color palette or follow-system mode.
    Theme(ThemeChoice),
    /// Bundled monospace grid font.
    Font(GridFont),
    /// Base font size in logical pixels.
    FontSize(f64),
    /// User zoom as a percentage.
    Zoom(f64),
}

impl AppearanceChange {
    /// Parse a command `name` and `value`, including a reset of the selected option.
    pub(crate) fn from_command(name: &str, value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        if value == "default" {
            let defaults = AppearanceSettings::default();
            return Some(match name {
                "theme" => Self::Theme(defaults.theme),
                "font" => Self::Font(defaults.font),
                "fontsize" => Self::FontSize(f64::from(defaults.base_size)),
                "zoom" => Self::Zoom(f64::from(defaults.zoom) * 100.),
                _ => return None,
            });
        }
        match name {
            "theme" => Self::theme(&value),
            "font" => Self::font(&value),
            "fontsize" => Self::font_size(value.parse().ok()?),
            "zoom" => Self::zoom(value.strip_suffix('%').unwrap_or(&value).parse().ok()?),
            _ => None,
        }
    }

    /// Resolve a supported theme `name` to its palette selection.
    fn theme(name: &str) -> Option<Self> {
        Some(Self::Theme(match name {
            "dark" => ThemeChoice::Dark,
            "light" => ThemeChoice::Light,
            "system" => ThemeChoice::System,
            _ => return None,
        }))
    }

    /// Resolve a supported font `name`, including the aliases accepted by the command.
    fn font(name: &str) -> Option<Self> {
        Some(Self::Font(match name {
            "rec" | "rec mono casual" => GridFont::RecMono,
            "maple" | "maple mono nl cn" => GridFont::MapleMono,
            _ => return None,
        }))
    }

    /// Accept a supported base font `size` in logical pixels.
    fn font_size(size: f64) -> Option<Self> {
        FONT_SIZE_RANGE
            .contains(&size)
            .then_some(Self::FontSize(size))
    }

    /// Accept a supported zoom `percent`.
    fn zoom(percent: f64) -> Option<Self> {
        ZOOM_RANGE.contains(&percent).then_some(Self::Zoom(percent))
    }

    /// Apply this change to the corresponding field of `appearance`.
    pub(crate) fn apply(self, appearance: &mut AppearanceSettings) {
        match self {
            Self::Theme(theme) => appearance.theme = theme,
            Self::Font(font) => appearance.font = font,
            Self::FontSize(size) => appearance.base_size = size as f32,
            Self::Zoom(percent) => appearance.zoom = (percent / 100.) as f32,
        }
    }

    /// Persist this field at `path`, keeping values written by other editor windows.
    pub(crate) fn save(self, path: &Path) -> Result<()> {
        let (key, value) = match self {
            Self::Theme(theme) => (
                "theme",
                Value::from(match theme {
                    ThemeChoice::Dark => "dark",
                    ThemeChoice::Light => "light",
                    ThemeChoice::System => "system",
                }),
            ),
            Self::Font(font) => (
                "font",
                Value::from(match font {
                    GridFont::RecMono => "rec",
                    GridFont::MapleMono => "maple",
                }),
            ),
            Self::FontSize(size) => ("font_size", number_value(size)),
            Self::Zoom(percent) => ("zoom", number_value(percent)),
        };
        settings::save_ui_value(path, key, value)
    }
}

/// Read a saved `item` as either integer or fractional TOML input.
fn number(item: &Item) -> Option<f64> {
    item.as_float()
        .or_else(|| item.as_integer().map(|n| n as f64))
}

/// Store whole numbers as TOML integers and keep fractional `number` values as floats.
fn number_value(number: f64) -> Value {
    if number.fract() == 0. {
        Value::from(number as i64)
    } else {
        Value::from(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save each command independently and restore the settings alongside the saved language.
    #[test]
    fn appearance_round_trips_with_other_ui_settings() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.toml");
        std::fs::write(&path, "[ui]\nlanguage = 'zh-CN' # keep this\n")?;
        let mut appearance = AppearanceSettings::default();
        for (name, value) in [
            ("theme", "light"),
            ("font", "Maple Mono NL CN"),
            ("fontsize", "18.5"),
            ("zoom", "125%"),
            ("theme", "system"),
        ] {
            let change =
                AppearanceChange::from_command(name, value).expect("valid appearance command");
            change.apply(&mut appearance);
            change.save(&path)?;
            assert_eq!(AppearanceSettings::load(&path)?, appearance);
        }
        let saved = std::fs::read_to_string(&path)?;
        assert!(saved.contains("language = 'zh-CN' # keep this"));
        assert!(saved.contains("zoom = 125\n"));
        assert!(saved.contains("font_size = 18.5\n"));
        AppearanceChange::from_command("zoom", "default")
            .unwrap()
            .save(&path)?;
        assert_eq!(AppearanceSettings::load(&path)?.zoom, 1.);
        Ok(())
    }

    /// Keep valid fields when a settings file omits or supplies an invalid appearance value.
    #[test]
    fn malformed_appearance_values_fall_back_independently() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.toml");
        assert_eq!(
            AppearanceSettings::load(&path)?,
            AppearanceSettings::default()
        );
        std::fs::write(
            &path,
            "[ui]\ntheme = 'light'\nfont = 'unknown'\nfont_size = nan\nzoom = 151\n",
        )?;
        assert_eq!(
            AppearanceSettings::load(&path)?,
            AppearanceSettings {
                theme: ThemeChoice::Light,
                ..Default::default()
            }
        );
        assert!(AppearanceChange::from_command("fontsize", "nan").is_none());
        assert!(AppearanceChange::from_command("zoom", "151%").is_none());
        Ok(())
    }
}

impl AppearanceSettings {
    /// Load appearance from `path`, using the default for each missing or invalid field.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let mut appearance = Self::default();
        settings::read_ui(path, |ui| {
            for change in [
                ui.get("theme")
                    .and_then(Item::as_str)
                    .and_then(AppearanceChange::theme),
                ui.get("font")
                    .and_then(Item::as_str)
                    .and_then(AppearanceChange::font),
                ui.get("font_size")
                    .and_then(number)
                    .and_then(AppearanceChange::font_size),
                ui.get("zoom")
                    .and_then(number)
                    .and_then(AppearanceChange::zoom),
            ]
            .into_iter()
            .flatten()
            {
                change.apply(&mut appearance);
            }
        })?;
        Ok(appearance)
    }
}
