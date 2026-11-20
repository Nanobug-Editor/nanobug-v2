//! Validated settings-page updates to Vim defaults and the views following them.
#[cfg(test)]
mod tests;

use super::{
    InputMappings, Scope, Setting, Vim, VimOptions, WindowOptions, set_option, valid_leader,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::path::Path;

/// Index the options that also have independently editable per-buffer or per-view copies.
pub(super) fn local_bit(name: &str) -> u16 {
    const LOCAL: &[&str] = &[
        "number",
        "relativenumber",
        "wrap",
        "scrollbind",
        "tabstop",
        "shiftwidth",
        "textwidth",
        "expandtab",
        "autoindent",
        "swapfile",
    ];
    LOCAL
        .iter()
        .position(|&item| item == name)
        .map_or(0, |bit| 1 << bit)
}

/// Locate an option in the portable `[vim.options]` representation.
fn table(name: &str) -> &'static [&'static str] {
    match name {
        "number" | "relativenumber" | "wrap" | "scrollbind" => &["vim", "options", "window"],
        "tabstop" | "shiftwidth" | "textwidth" | "expandtab" | "autoindent" | "swapfile" => {
            &["vim", "options", "buffer"]
        }
        _ => &["vim", "options"],
    }
}

impl VimOptions {
    /// Read a panel field in the native TOML structure; wrap has an automatic third state.
    pub fn preference(&self, name: &str) -> Result<Value> {
        let snapshot = serde_json::to_value(self)?;
        let mut container = &snapshot;
        for &key in &table(name)[2..] {
            container = &container[key];
        }
        let value = container
            .get(name)
            .with_context(|| format!("Unknown Vim option {name}"))?;
        if name == "wrap" {
            return Ok(Value::from(match value.as_bool() {
                Some(true) => "on",
                Some(false) => "off",
                None => "auto",
            }));
        }
        Ok(value.clone())
    }

    /// Validate with the same assignments as :setglobal, retaining unrelated option state.
    fn set_preference(&mut self, name: &str, value: &Value) -> Result<()> {
        let mut window = self.window;
        let mut buffer = self.buffer.clone();
        let number;
        let setting = match (name, value) {
            ("mapleader", Value::String(leader)) => {
                ensure!(valid_leader(leader), "Invalid mapleader");
                self.mapleader.clone_from(leader);
                return Ok(());
            }
            ("filetype_detection", Value::Bool(on)) => {
                self.filetype_detection = *on;
                return Ok(());
            }
            ("wrap", Value::String(choice)) => match choice.as_str() {
                "on" => Setting::Boolean(true),
                "off" => Setting::Boolean(false),
                "auto" => Setting::Reset,
                _ => bail!("Invalid wrap setting"),
            },
            (_, Value::Bool(on)) => Setting::Boolean(*on),
            (_, Value::String(value)) => Setting::Assign(value),
            (_, Value::Number(value)) => {
                number = value.to_string();
                Setting::Assign(&number)
            }
            _ => bail!("Invalid value for {name}"),
        };
        ensure!(
            matches!(
                set_option(
                    name,
                    setting,
                    Scope::Global,
                    false,
                    self,
                    &mut window,
                    &mut buffer
                ),
                Ok(None)
            ),
            "Invalid value for {name}"
        );
        Ok(())
    }
}

impl InputMappings {
    /// Validate a whole page before writing; preserve other Vim options, maps and annotations.
    pub fn save_option_preferences(
        &self,
        changes: &[(String, Value)],
        path: Option<&Path>,
        restore: Option<crate::app_data::settings::RestoreFields<'_>>,
    ) -> Result<VimOptions> {
        let mut next = self.options();
        let mut fields = Vec::new();
        for (name, value) in changes {
            next.set_preference(name, value)?;
            let mut keys = table(name).to_vec();
            keys.push(name);
            let value = match (name.as_str(), value) {
                ("wrap", _) => next.window.wrap.map(toml_edit::Value::from),
                (_, Value::String(v)) => Some(v.as_str().into()),
                (_, Value::Bool(v)) => Some((*v).into()),
                (_, Value::Number(v)) => Some(v.as_i64().context("Integer out of range")?.into()),
                _ => bail!("Invalid value for {name}"),
            };
            fields.push((keys, value));
        }
        if let Some(path) = path {
            let writes = fields
                .iter()
                .map(|(keys, value)| (keys.as_slice(), value.clone()))
                .collect::<Vec<_>>();
            crate::app_data::settings::save_fields(path, &writes, restore)?;
        }
        self.set_ex_options(next.clone());
        Ok(next)
    }
}

impl WindowOptions {
    /// Synchronize inherited per-view defaults after a preferences edit.
    pub fn follow_preferences(&mut self, next: &VimOptions, changes: &[(String, Value)]) {
        for (key, _) in changes {
            if self.local_defaults & local_bit(key) != 0 {
                continue;
            }
            match key.as_str() {
                "number" => self.number = next.window.number,
                "relativenumber" => self.relativenumber = next.window.relativenumber,
                "wrap" => self.wrap = next.window.wrap,
                "scrollbind" => self.scrollbind = next.window.scrollbind,
                _ => (),
            }
        }
    }
}

impl Vim {
    /// Refresh inherited values without overwriting a buffer's explicit :setlocal overrides.
    pub fn follow_preferences(&mut self, next: &VimOptions, changes: &[(String, Value)]) {
        if let Some(view) = &mut self.view_options {
            view.follow_preferences(next, changes);
        }
        if let Some(buffer) = &self.buffer_options {
            let mut local = buffer.write().unwrap();
            for (key, _) in changes {
                if local.local_defaults & local_bit(key) != 0 {
                    continue;
                }
                match key.as_str() {
                    "tabstop" => {
                        local.tabstop = next.buffer.tabstop;
                        self.doc.set_tabstop(local.tabstop);
                    }
                    "shiftwidth" => local.shiftwidth = next.buffer.shiftwidth,
                    "textwidth" => local.textwidth = next.buffer.textwidth,
                    "expandtab" => local.expandtab = next.buffer.expandtab,
                    "autoindent" => local.autoindent = next.buffer.autoindent,
                    "swapfile" => local.swapfile = next.buffer.swapfile,
                    "hlsearch" => self.search_highlights_hidden = false,
                    _ => (),
                }
            }
        }
    }
}
