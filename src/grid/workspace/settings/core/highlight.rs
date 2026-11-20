//! Present the named syntax groups served to language plugins and rendered by the editor.

use super::{Kind, Page, Workspace, choices, field, page};
use anyhow::{Context as _, Result, bail, ensure};

use serde_json::{Value, json};

use crate::ui::theme::{syntax_example, syntax_section};
use nanobug_plugin_sdk::language::SyntaxKind;
const UNDERLINES: &[(&str, &str)] = &[
    ("none", "settings_core.none"),
    ("underline", "settings_core.single"),
    ("underdouble", "settings_core.double"),
    ("undercurl", "settings_core.curl"),
    ("underdotted", "settings_core.dotted"),
    ("underdashed", "settings_core.dashed"),
];
const DECORATIONS: &[(&str, &str)] = &[
    ("bold", "settings_core.bold"),
    ("italic", "settings_core.italic"),
    ("strikethrough", "settings_core.strikethrough"),
    ("overline", "settings_core.overline"),
];

impl Workspace {
    pub(in crate::grid::workspace::settings) fn highlight_settings_pages(&self) -> Vec<Page> {
        SyntaxKind::ALL
            .iter()
            .map(|&kind| {
                let group = kind.group();
                let mut path = vec!["settings_core.appearance", "settings_core.syntax"];
                path.extend(syntax_section(kind));
                let mut ancestry: Vec<_> = std::iter::successors(Some(kind), |role| role.parent())
                    .map(|k| syntax_example(k).0)
                    .collect();
                ancestry.reverse();
                path.extend(ancestry);
                let color = self
                    .input_mappings
                    .highlight_foreground(group)
                    .map(|n| format!("#{n:06x}"))
                    .unwrap_or_default();
                let a = self
                    .input_mappings
                    .highlight_style(group)
                    .map(|style| style.attributes)
                    .unwrap_or_default();
                let inherited = self
                    .input_mappings
                    .inherited_highlight_style(group)
                    .unwrap_or_default();
                let defaults = inherited.attributes;
                page(
                    &format!("core.highlight.{group}"),
                    &path,
                    vec![
                        field(
                            "reverse",
                            "settings_core.reverse",
                            Kind::Toggle,
                            json!(a.reverse),
                            json!(defaults.reverse),
                        ),
                        field(
                            "underline",
                            "settings_core.underline",
                            Kind::Select(choices(UNDERLINES)),
                            json!(a.underline_shape()),
                            json!(defaults.underline_shape()),
                        ),
                        field(
                            "decorations",
                            "settings_core.decorations",
                            Kind::Multi(choices(DECORATIONS)),
                            json!(a.decorations()),
                            json!(defaults.decorations()),
                        ),
                        field(
                            "foreground",
                            "settings_core.highlight_fg",
                            Kind::Color {
                                inherited: inherited.foreground,
                            },
                            json!(color),
                            json!(""),
                        ),
                    ],
                )
            })
            .collect()
    }

    /// Commit the foreground and GUI attributes together, preserving the settings snapshot on rollback.
    pub(super) fn write_highlight_settings(
        &self,
        group: &str,
        opening: &Page,
        changes: &[(String, Value)],
    ) -> Result<()> {
        ensure!(
            SyntaxKind::ALL.iter().any(|k| k.group() == group),
            "Unknown syntax group {group}"
        );
        let mut a = self
            .input_mappings
            .highlight_style(group)
            .context("Unknown syntax group")?
            .attributes;
        let mut color = self.input_mappings.highlight_foreground(group);
        for (id, value) in changes {
            match id.as_str() {
                "foreground" => {
                    let input = value.as_str().context("Expected a color")?.trim();
                    color = if input.is_empty() {
                        None
                    } else {
                        let hex = input
                            .strip_prefix('#')
                            .filter(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                            .context("Expected #RRGGBB")?;
                        Some(u32::from_str_radix(hex, 16)?)
                    };
                }
                "reverse" => a.reverse = value.as_bool().context("Expected a reverse flag")?,
                "underline" => {
                    let shape = value.as_str().context("Expected an underline name")?;
                    ensure!(
                        UNDERLINES.iter().any(|&(id, _)| id == shape),
                        "Unknown underline {shape}"
                    );
                    a.underline = shape == "underline";
                    a.underdouble = shape == "underdouble";
                    a.undercurl = shape == "undercurl";
                    a.underdotted = shape == "underdotted";
                    a.underdashed = shape == "underdashed";
                }
                "decorations" => {
                    let list = value
                        .as_array()
                        .context("Expected a text decoration list")?;
                    ensure!(
                        list.iter()
                            .all(|v| DECORATIONS.iter().any(|&(id, _)| v.as_str() == Some(id))),
                        "Unknown text decoration"
                    );
                    let has = |name: &str| list.iter().any(|v| v.as_str() == Some(name));
                    a.bold = has("bold");
                    a.italic = has("italic");
                    a.strikethrough = has("strikethrough");
                    a.overline = has("overline");
                }
                _ => bail!("Unknown highlight control {id}"),
            }
        }
        let restored = opening.fields.iter().all(|field| match field.id.as_str() {
            "foreground" => field.value.as_str().is_some_and(|v| {
                v.eq_ignore_ascii_case(&color.map(|c| format!("#{c:06x}")).unwrap_or_default())
            }),
            "decorations" => field.value == json!(a.decorations()),
            "underline" => field.value == a.underline_shape(),
            "reverse" => field.value == a.reverse,
            _ => false,
        });
        let key = group.to_ascii_lowercase();
        let names = [key.as_str()];
        self.input_mappings.save_highlight_appearance(
            group,
            a,
            color,
            self.vim_settings_path.as_deref(),
            restored.then_some(nanobug::app_data::settings::RestoreFields {
                snapshot: &self.core_settings_baseline.vim,
                names: &names,
            }),
        )
    }
}
