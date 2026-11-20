//! Ex highlight defaults backed by the native GUI palette and SDK syntax roles.

use super::{Style, ThemeComponent, palette, syntax_color};
use nanobug::vim::{HighlightStyle, InputMappings};
use nanobug_plugin_sdk::language::SyntaxKind;

/// Familiar Ex groups retain their historical role or inherit a more specific SDK group.
const ALIASES: &[(&str, SyntaxKind)] = &[
    ("Statement", SyntaxKind::Keyword),
    ("Identifier", SyntaxKind::Function),
    ("PreProc", SyntaxKind::Attribute),
    ("StorageClass", SyntaxKind::Type),
    ("Structure", SyntaxKind::Type),
    ("Typedef", SyntaxKind::Type),
    ("Include", SyntaxKind::Import),
    ("Define", SyntaxKind::Directive),
    ("PreCondit", SyntaxKind::Directive),
];

impl Style {
    /// Update the workspace's default named groups against this selected appearance.
    pub(crate) fn update_ex_highlight_palette(self, mappings: &InputMappings) {
        let editor = self.for_component(ThemeComponent::Editor);
        let gutter = self.for_component(ThemeComponent::Gutter);
        let footer = self.for_component(ThemeComponent::Footer);
        let foreground = |rgb| HighlightStyle {
            foreground: Some(rgb),
            ..Default::default()
        };
        let background = |rgb| HighlightStyle {
            background: Some(rgb),
            ..Default::default()
        };
        let tint = |rgba: u32| HighlightStyle {
            background: Some(rgba >> 8),
            blend: Some(100 - ((rgba & 0xff) * 100 / 255) as u8),
            ..Default::default()
        };
        let normal = HighlightStyle {
            foreground: Some(editor.foreground(palette::TEXT)),
            background: Some(editor.background() >> 8),
            ..Default::default()
        };
        let base = [
            ("Normal", normal),
            ("NormalNC", normal),
            ("Search", background(editor.foreground(palette::GOLD))),
            ("IncSearch", background(editor.foreground(palette::ACCENT))),
            ("CurSearch", background(editor.foreground(palette::ACCENT))),
            ("Visual", tint(editor.tint(palette::SELECTION))),
            ("CursorLine", tint(editor.tint(palette::CURRENT))),
            (
                "Cursor",
                HighlightStyle {
                    foreground: Some(editor.foreground(palette::CURSOR_TEXT)),
                    background: Some(editor.cursor()),
                    ..Default::default()
                },
            ),
            ("LineNr", foreground(gutter.foreground(palette::MUTED))),
            ("SignColumn", foreground(gutter.foreground(palette::MUTED))),
            (
                "CursorLineSign",
                foreground(gutter.foreground(palette::TEXT)),
            ),
            ("CursorLineNr", foreground(gutter.foreground(palette::GOLD))),
            ("NonText", foreground(editor.foreground(palette::MUTED))),
            (
                "ErrorMsg",
                foreground(footer.foreground(palette::ERROR_TEXT)),
            ),
            (
                "WarningMsg",
                foreground(footer.foreground(palette::WARNING_TEXT)),
            ),
            ("ModeMsg", foreground(footer.foreground(palette::TEXT))),
            ("MoreMsg", foreground(footer.foreground(palette::ACCENT))),
            ("Question", foreground(footer.foreground(palette::ACCENT))),
            ("MsgArea", foreground(footer.foreground(palette::TEXT))),
            ("StatusLine", foreground(footer.foreground(palette::TEXT))),
            ("Pmenu", normal),
            ("PmenuSel", tint(editor.tint(palette::SELECTION))),
            ("Directory", foreground(editor.foreground(palette::ACCENT))),
            ("Title", foreground(editor.foreground(palette::ACCENT))),
            ("Special", foreground(editor.foreground(palette::GOLD))),
            ("Todo", background(editor.foreground(palette::GOLD))),
            ("MatchParen", background(editor.foreground(palette::ACCENT))),
            ("Error", foreground(editor.foreground(palette::ERROR_TEXT))),
        ];
        let mut entries = base.to_vec();
        let mut parents = Vec::new();
        for &kind in SyntaxKind::ALL {
            let mut style = foreground(editor.foreground(syntax_color(kind)));
            if matches!(
                kind,
                SyntaxKind::Readonly | SyntaxKind::Static | SyntaxKind::Deprecated
            ) {
                style.foreground =
                    palette::modifier_foreground(editor.light, editor.component, kind);
                style.attributes.strikethrough = kind == SyntaxKind::Deprecated;
            }
            entries.extend([(kind.group(), style), (kind.capture_group(), style)]);
            parents.push((kind.capture_group(), kind.group()));
            if let Some(parent) = kind.parent() {
                parents.push((kind.group(), parent.capture_group()));
            }
        }
        for &(alias, kind) in ALIASES {
            entries.push((alias, foreground(editor.foreground(syntax_color(kind)))));
        }
        // These legacy overrides were part of the original seven-role rendering contract.
        parents.extend([
            ("Keyword", "Statement"),
            ("Function", "Identifier"),
            ("Attribute", "PreProc"),
            ("String", "@constant"),
        ]);
        mappings.set_highlight_palette(entries);
        mappings.set_highlight_inheritance(parents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn methods_inherit_styles_and_override_colors_independently() -> anyhow::Result<()> {
        let mappings = InputMappings::default();
        Style::default().update_ex_highlight_palette(&mappings);
        let mut attributes = nanobug::vim::HighlightAttributes::default();
        attributes.italic = true;
        mappings.save_highlight_appearance("Function", attributes, Some(0x123456), None, None)?;
        let method = || mappings.highlight_style("@function.method").unwrap();
        assert_eq!(method().foreground, Some(0x123456));
        assert!(method().attributes.italic);
        mappings.save_highlight_appearance("Method", attributes, Some(0x789abc), None, None)?;
        assert_eq!(method().foreground, Some(0x789abc));
        mappings.save_highlight_appearance("Method", attributes, None, None, None)?;
        assert_eq!(method().foreground, Some(0x123456));
        assert_eq!(mappings.highlight_foreground("Method"), None);
        Ok(())
    }
}
