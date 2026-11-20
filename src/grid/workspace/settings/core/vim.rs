//! Group Vim settings by editing capability while taking values and defaults from Vim itself.

use super::{Kind, Page, Workspace, choices, field, page};
use anyhow::Result;
use nanobug::vim::VimOptions;

impl Workspace {
    pub(super) fn vim_settings_pages(&self) -> Result<Vec<Page>> {
        let current = self.input_mappings.options();
        let defaults = VimOptions::default();
        let option = |id: &str, caption, kind| -> Result<_> {
            Ok(field(
                id,
                caption,
                kind,
                current.preference(id)?,
                defaults.preference(id)?,
            ))
        };
        Ok(vec![
            page(
                "core.vim.indent",
                &["settings_core.edit", "settings_core.indent"],
                vec![
                    option(
                        "tabstop",
                        "settings_core.tabstop",
                        Kind::Number { min: 1, max: 9999 },
                    )?,
                    option(
                        "shiftwidth",
                        "settings_core.shiftwidth",
                        Kind::Number { min: 0, max: 9999 },
                    )?,
                    option("expandtab", "settings_core.expandtab", Kind::Toggle)?,
                    option("autoindent", "settings_core.autoindent", Kind::Toggle)?,
                ],
            ),
            page(
                "core.vim.display",
                &["settings_core.edit", "settings_core.display"],
                vec![
                    option("number", "settings_core.number", Kind::Toggle)?,
                    option(
                        "relativenumber",
                        "settings_core.relativenumber",
                        Kind::Toggle,
                    )?,
                    option(
                        "wrap",
                        "settings_core.wrap",
                        Kind::Select(choices(&[
                            ("auto", "settings_core.auto"),
                            ("on", "settings_core.on"),
                            ("off", "settings_core.off"),
                        ])),
                    )?,
                    option(
                        "textwidth",
                        "settings_core.textwidth",
                        Kind::Number { min: 0, max: 9999 },
                    )?,
                ],
            ),
            page(
                "core.vim.keys",
                &["settings_core.edit", "settings_core.keys"],
                vec![
                    option("mapleader", "settings_core.mapleader", Kind::Text)?,
                    option("timeout", "settings_core.timeout", Kind::Toggle)?,
                    option(
                        "timeoutlen",
                        "settings_core.timeoutlen",
                        Kind::Number {
                            min: 0,
                            max: 4294967295,
                        },
                    )?,
                ],
            ),
            page(
                "core.vim.match",
                &["settings_core.search", "settings_core.match"],
                vec![
                    option("ignorecase", "settings_core.ignorecase", Kind::Toggle)?,
                    option("smartcase", "settings_core.smartcase", Kind::Toggle)?,
                ],
            ),
            page(
                "core.vim.results",
                &["settings_core.search", "settings_core.results"],
                vec![
                    option("hlsearch", "settings_core.hlsearch", Kind::Toggle)?,
                    option("incsearch", "settings_core.incsearch", Kind::Toggle)?,
                ],
            ),
            page(
                "core.vim.viewport",
                &["settings_core.windows", "settings_core.viewport"],
                vec![
                    option(
                        "scrolloff",
                        "settings_core.scrolloff",
                        Kind::Number {
                            min: 0,
                            max: i64::MAX,
                        },
                    )?,
                    option("scrollbind", "settings_core.scrollbind", Kind::Toggle)?,
                ],
            ),
            page(
                "core.vim.files",
                &["settings_core.recovery", "settings_core.files"],
                vec![
                    option("autoread", "settings_core.autoread", Kind::Toggle)?,
                    option("writeany", "settings_core.writeany", Kind::Toggle)?,
                    option("swapfile", "settings_core.swapfile", Kind::Toggle)?,
                    option(
                        "filetype_detection",
                        "settings_core.filetype_detection",
                        Kind::Toggle,
                    )?,
                ],
            ),
            page(
                "core.vim.paths",
                &["settings_core.recovery", "settings_core.paths"],
                vec![
                    option("path", "settings_core.path", Kind::Text)?,
                    option("cdhome", "settings_core.cdhome", Kind::Toggle)?,
                    option("cdpath", "settings_core.cdpath", Kind::Text)?,
                    option("viewdir", "settings_core.viewdir", Kind::Text)?,
                    option("runtimepath", "settings_core.runtimepath", Kind::Text)?,
                ],
            ),
            page(
                "core.vim.messages",
                &["settings_core.edit", "settings_core.messages"],
                vec![
                    option(
                        "verbose",
                        "settings_core.verbose",
                        Kind::Number {
                            min: 0,
                            max: 4294967295,
                        },
                    )?,
                    option("verbosefile", "settings_core.verbosefile", Kind::Text)?,
                ],
            ),
        ])
    }
}
