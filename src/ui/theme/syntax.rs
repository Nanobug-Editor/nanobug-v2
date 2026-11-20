//! Localized syntax settings and examples for the SDK's portable roles.
use SyntaxKind::*;
use nanobug_plugin_sdk::language::SyntaxKind;

/// Keep samples and localization keys exhaustive when the SDK grows.
macro_rules! examples {
    ($( $kind:ident => $sample:literal, )+) => {
        pub(crate) fn syntax_example(kind: SyntaxKind) -> (&'static str, &'static str) {
            match kind { $( $kind => (concat!("settings_core.", stringify!($kind)), $sample), )+ }
        }
    }
}
examples! {
    Comment => "// Nanobug",
    DocComment => "/// Description",
    TodoComment => "TODO",
    Keyword => "let",
    Conditional => "if",
    Repeat => "while",
    Return => "return",
    Import => "import",
    Exception => "catch",
    Directive => "#include",
    Modifier => "public",
    Type => "User",
    BuiltinType => "usize",
    Class => "User",
    Struct => "Point",
    Interface => "Display",
    Enum => "Color",
    TypeParameter => "T",
    Function => "main",
    Method => "save",
    Constructor => "User",
    Macro => "println!",
    BuiltinFunction => "len",
    String => "\"Nanobug\"",
    Character => "'a'",
    Regexp => "/ab+/",
    Escape => "\\n",
    Interpolation => "${name}",
    Constant => "MAX_SIZE",
    Number => "42",
    Float => "3.14",
    Boolean => "true",
    Null => "null",
    EnumMember => "Red",
    Attribute => "#[derive(Debug)]",
    Variable => "count",
    Parameter => "limit",
    Field => "name",
    BuiltinVariable => "self",
    Module => "std",
    Operator => "+=",
    Punctuation => ":",
    Bracket => "()",
    Delimiter => ",",
    Lifetime => "'a",
    Label => "'outer",
    Tag => "button",
    Component => "UserCard",
    TagAttribute => "disabled",
    TemplateDirective => "v-if",
    CssSelector => ".card",
    CssProperty => "color",
    ConfigKey => "port",
    SqlTable => "users",
    SqlColumn => "user_id",
    SqlAlias => "u",
    SqlParameter => "$1",
    MarkupHeading => "# Heading",
    MarkupEmphasis => "*text*",
    MarkupStrong => "**text**",
    MarkupStrike => "~~text~~",
    MarkupLink => "Nanobug",
    MarkupUrl => "https://example.org",
    MarkupCode => "`code`",
    MarkupCodeBlock => "fn main() {}",
    MarkupQuote => "> quote",
    MarkupList => "- [x]",
    MarkupTable => "Column",
    MarkupMarker => "---",
    Readonly => "MAX_SIZE",
    Static => "shared",
    Deprecated => "old_method",
}

/// Optional sections for syntax that occurs only in specific document formats.
pub(crate) fn syntax_section(kind: SyntaxKind) -> Option<&'static str> {
    Some(match kind {
        Tag | Component | TagAttribute | TemplateDirective | CssSelector | CssProperty => {
            "settings_core.web_syntax"
        }
        SqlTable | SqlColumn | SqlAlias | SqlParameter => "settings_core.sql_syntax",
        MarkupHeading | MarkupEmphasis | MarkupStrong | MarkupStrike | MarkupLink | MarkupUrl
        | MarkupCode | MarkupCodeBlock | MarkupQuote | MarkupList | MarkupTable | MarkupMarker => {
            "settings_core.markup_syntax"
        }
        Readonly | Static | Deprecated => "settings_core.semantic_syntax",
        _ => return None,
    })
}
