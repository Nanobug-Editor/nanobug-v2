//! Appearance settings reuse command validation, native font loading and live remeasurement.

use super::{Kind, Page, Workspace, choices, field, page};
use crate::ui::theme::{AppearanceChange, AppearanceSettings, GridFont, ThemeChoice};
use anyhow::{Context as _, Result, bail};
use gpui::{Context, Window};
use nanobug::{
    app_data::settings,
    i18n::{self, Locale},
};
use serde_json::{Value, json};

fn theme_name(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::Dark => "dark",
        ThemeChoice::Light => "light",
        ThemeChoice::System => "system",
    }
}

fn font_name(font: GridFont) -> &'static str {
    match font {
        GridFont::RecMono => "rec",
        GridFont::MapleMono => "maple",
    }
}

impl Workspace {
    /// Describe appearance and language in the same units used by their public commands.
    pub(super) fn appearance_settings_pages(&self) -> Vec<Page> {
        let a = self.appearance;
        let d = AppearanceSettings::default();
        vec![
            page(
                "core.appearance.theme",
                &["settings_core.appearance", "settings_core.theme"],
                vec![field(
                    "theme",
                    "settings_core.theme",
                    Kind::Select(choices(&[
                        ("dark", "settings_core.dark"),
                        ("light", "settings_core.light"),
                        ("system", "settings_core.system"),
                    ])),
                    json!(theme_name(a.theme)),
                    json!(theme_name(d.theme)),
                )],
            ),
            page(
                "core.appearance.font",
                &["settings_core.appearance", "settings_core.font_zoom"],
                vec![
                    field(
                        "font",
                        "settings_core.font",
                        Kind::Select(choices(&[
                            ("rec", "settings_core.rec"),
                            ("maple", "settings_core.maple"),
                        ])),
                        json!(font_name(a.font)),
                        json!(font_name(d.font)),
                    ),
                    field(
                        "font_size",
                        "settings_core.font_size",
                        Kind::Decimal { min: 12, max: 24 },
                        json!(a.base_size),
                        json!(d.base_size),
                    ),
                    field(
                        "zoom",
                        "settings_core.zoom",
                        Kind::Decimal { min: 75, max: 150 },
                        json!(a.zoom * 100.),
                        json!(d.zoom * 100.),
                    ),
                ],
            ),
            page(
                "core.appearance.language",
                &["settings_core.appearance", "settings_core.language"],
                vec![field(
                    "language",
                    "settings_core.language",
                    Kind::Select(choices(&[
                        ("en", "language.english"),
                        ("zh-CN", "language.chinese"),
                    ])),
                    json!(i18n::locale().tag()),
                    json!(Locale::English.tag()),
                )],
            ),
        ]
    }

    /// Prepare every field and optional font before touching either the file or live appearance.
    pub(super) fn write_appearance_settings(
        &mut self,
        changes: &[(String, Value)],
        restored: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let mut next = self.appearance;
        let mut language = None;
        let mut fields = Vec::new();
        for (id, value) in changes {
            if id == "language" {
                language = Some(match value.as_str() {
                    Some("en") => Locale::English,
                    Some("zh-CN") => Locale::SimplifiedChinese,
                    _ => bail!("Unsupported interface language"),
                });
                fields.push((["ui", "language"], Some(value.as_str().unwrap().into())));
                continue;
            }
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            let command = if id == "font_size" { "fontsize" } else { id };
            let change = AppearanceChange::from_command(command, &value)
                .with_context(|| format!("Invalid {id}: {value}"))?;
            let (key, value) = change.field();
            fields.push((["ui", key], Some(value)));
            change.apply(&mut next);
        }
        if next.font == GridFont::MapleMono {
            GridFont::ensure_maple(self.glyphs.as_ref())?;
        }
        if let Some(path) = self.appearance_settings_path.as_deref() {
            let fields = fields
                .iter()
                .map(|(keys, value)| (keys.as_slice(), value.clone()))
                .collect::<Vec<_>>();
            settings::save_fields(
                path,
                &fields,
                Some(settings::RestoreFields {
                    snapshot: &self.core_settings_baseline.appearance,
                    names: restored,
                }),
            )?;
        }
        if let Some(language) = language {
            i18n::set_locale(language);
        }
        self.apply_appearance(next, window, cx);
        Ok(())
    }
}
