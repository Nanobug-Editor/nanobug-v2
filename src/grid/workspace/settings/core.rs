//! Connect each core settings page to the runtime object and portable file that own it.

mod appearance;
mod highlight;
mod vim;

use super::super::Workspace;
use crate::grid::windows::settings::{Choice, Field, Kind, Page};
use anyhow::{Result, bail};
use gpui::{Context, Window};
use nanobug::app_data::settings::{RestoreFields, SettingsSnapshot};
use nanobug::i18n;
use serde_json::Value;

/// Opening representations for reliable cancellation of edits to portable settings.
#[derive(Default)]
pub(in crate::grid::workspace) struct CoreBaseline {
    appearance: SettingsSnapshot,
    vim: SettingsSnapshot,
}

/// Construct one localized tree leaf; paths start at the core settings root.
fn page(id: &str, path: &[&'static str], fields: Vec<Field>) -> Page {
    Page {
        id: id.into(),
        path: std::iter::once("settings_core.root")
            .chain(path.iter().copied())
            .map(|key| i18n::text(key).to_owned())
            .collect(),
        fields,
    }
}

/// Tie stable field and choice IDs to localized user-visible captions.
fn field(id: &str, caption: &'static str, kind: Kind, value: Value, default: Value) -> Field {
    Field {
        id: id.into(),
        label: i18n::text(caption).into(),
        kind,
        value,
        default,
    }
}

fn choices(items: &[(&str, &'static str)]) -> Vec<Choice> {
    items
        .iter()
        .map(|(id, key)| Choice {
            id: (*id).into(),
            label: i18n::text(key).into(),
        })
        .collect()
}

impl Workspace {
    /// Take the persistence baseline once per dialog, even if its locale changes while open.
    pub(super) fn capture_core_settings_baseline(&mut self) -> Result<()> {
        self.core_settings_baseline = CoreBaseline {
            appearance: SettingsSnapshot::load(self.appearance_settings_path.as_deref())?,
            vim: SettingsSnapshot::load(self.vim_settings_path.as_deref())?,
        };
        Ok(())
    }
    /// Capture current values and system defaults from the appearance and Vim owners.
    pub(super) fn core_settings_pages(&self) -> Result<Vec<Page>> {
        let mut pages = self.vim_settings_pages()?;
        pages.extend(self.appearance_settings_pages());
        pages.extend(self.highlight_settings_pages());
        Ok(pages)
    }

    /// Write all changed values of one page before publishing them to dependent views.
    pub(super) fn write_core_settings(
        &mut self,
        page: &Page,
        changes: &[(String, Value)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let restored: Vec<&str> = changes
            .iter()
            .filter_map(|(id, value)| {
                page.fields
                    .iter()
                    .any(|field| field.id == *id && field.value == *value)
                    .then_some(id.as_str())
            })
            .collect();
        if page.id.starts_with("core.vim.") {
            let next = self.input_mappings.save_option_preferences(
                changes,
                self.vim_settings_path.as_deref(),
                Some(RestoreFields {
                    snapshot: &self.core_settings_baseline.vim,
                    names: &restored,
                }),
            )?;
            for project in self.projects.iter_mut() {
                project.follow_vim_preferences(&next, changes);
            }
        } else if page.id.starts_with("core.appearance.") {
            self.write_appearance_settings(changes, &restored, window, cx)?;
        } else if let Some(group) = page.id.strip_prefix("core.highlight.") {
            self.write_highlight_settings(group, page, changes)?;
        } else {
            bail!("Unknown core settings page {}", page.id);
        }
        Ok(())
    }
}
