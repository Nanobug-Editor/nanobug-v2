//! Ex highlight defaults backed by the native GUI palette and SDK syntax roles.

use super::{Style, ThemeComponent, palette, syntax_color};
use nanobug::vim::{HighlightStyle, InputMappings};
use nanobug_plugin_sdk::language::SyntaxKind;

/// Return the Ex groups applicable to an SDK `kind` in order of specificity.
pub(crate) fn syntax_highlight_groups(kind: SyntaxKind) -> &'static [&'static str] {
    use SyntaxKind::*;
    match kind {
        Keyword => &["@keyword", "Keyword", "Statement"],
        Type => &["@type", "Type"],
        Function => &["@function", "Function", "Identifier"],
        String => &["@string", "String", "Constant"],
        Constant => &["@constant", "Constant"],
        Comment => &["@comment", "Comment"],
        Attribute => &["@attribute", "Attribute", "PreProc"],
    }
}

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
        let syntax = [
            SyntaxKind::String,
            SyntaxKind::Constant,
            SyntaxKind::Keyword,
            SyntaxKind::Type,
            SyntaxKind::Function,
            SyntaxKind::Comment,
            SyntaxKind::Attribute,
        ]
        .into_iter()
        .flat_map(|kind| {
            let style = foreground(editor.foreground(syntax_color(kind)));
            syntax_highlight_groups(kind)
                .iter()
                .map(move |name| (*name, style))
        });
        let aliases = [
            (
                SyntaxKind::Constant,
                &["Character", "Number", "Boolean", "Float"][..],
            ),
            (
                SyntaxKind::Keyword,
                &["Conditional", "Repeat", "Label", "Operator", "Exception"][..],
            ),
            (
                SyntaxKind::Type,
                &["StorageClass", "Structure", "Typedef"][..],
            ),
            (
                SyntaxKind::Attribute,
                &["Include", "Define", "Macro", "PreCondit"][..],
            ),
        ]
        .into_iter()
        .flat_map(|(kind, names)| {
            let style = foreground(editor.foreground(syntax_color(kind)));
            names.iter().map(move |name| (*name, style))
        });
        mappings.set_highlight_palette(base.into_iter().chain(syntax).chain(aliases));
    }
}
