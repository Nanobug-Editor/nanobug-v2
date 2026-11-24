//! Read editable palettes while keeping retained color roles independent of their RGB values.

use anyhow::{Context, Result, bail, ensure};
use nanobug_plugin_sdk::language::SyntaxKind;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    io::{ErrorKind, Write},
    path::Path,
    sync::{LazyLock, OnceLock},
};

/// Commented templates used for first launch, migration and runtime fallback.
const DARK: &str = include_str!("../../../assets/theme-dark.toml");
const LIGHT: &str = include_str!("../../../assets/theme-light.toml");
/// The previous single-file format remains a source for missing TOML files.
const LEGACY_FILE: &str = "theme.json";
/// Retained role IDs use a reserved, transparent RGBA range outside literal RGB.
const ROLE_START: u32 = 0x4e42_0000;

/// Declare retained roles with their configuration key and hexadecimal digit count.
macro_rules! color_roles {
    ($($role:ident: $key:literal, $digits:literal);+ $(;)?) => {
        // Keep the same spelling as the constants retained by the grid.
        #[allow(non_camel_case_types)]
        #[repr(usize)]
        enum Role { $($role),+ }

        $(pub(crate) const $role: u32 = ROLE_START + ((Role::$role as u32) << 8);)+

        const FIELDS: &[(&str, usize)] = &[$(($key, $digits)),+];
    };
}

color_roles! {
    TEXT: "text", 6;
    MUTED: "muted", 6;
    ACCENT: "accent", 6;
    GOLD: "gold", 6;
    WARNING_BORDER: "warning_border", 6;
    WARNING_TEXT: "warning_text", 6;
    ERROR_BORDER: "error_border", 6;
    ERROR_TEXT: "error_text", 6;
    BACKGROUND: "background", 8;
    CURSOR_FILL: "cursor_fill", 6;
    CURSOR_TEXT: "cursor_text", 6;
    SELECTION: "selection", 8;
    CURRENT: "current", 8;
    TREE_GUIDE: "tree_guide", 8;
    ACTIVE_FILE: "active_file", 8;
    PREEDIT: "preedit", 8;
    FOOTER_BLUE: "footer_blue", 8;
    FOOTER_DETAILS: "footer_details", 8;
    FOOTER_VISUAL: "footer_visual", 8;
    FOOTER_INSERT: "footer_insert", 8;
    SHADOW: "shadow", 8;
    PLUGIN_ERROR: "plugin_error", 8;
    SYNTAX_KEYWORD: "syntax_keyword", 6;
    SYNTAX_TYPE: "syntax_type", 6;
    SYNTAX_FUNCTION: "syntax_function", 6;
    SYNTAX_STRING: "syntax_string", 6;
    SYNTAX_CONSTANT: "syntax_constant", 6;
    SYNTAX_COMMENT: "syntax_comment", 6;
    SYNTAX_ATTRIBUTE: "syntax_attribute", 6;
    WARNING_UNDERLINE: "warning_underline", 6;
    ERROR_UNDERLINE: "error_underline", 6;
}

/// An independently themed region, ordered for indexing into `Palette::components`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThemeComponent {
    /// Editor chrome and text area.
    Editor,
    /// Line numbers and diagnostics beside the editor text.
    Gutter,
    /// File explorer chrome.
    Explorer,
    /// Indented file tree inside the explorer.
    ExplorerTree,
    /// Workspace status bar.
    Footer,
}

impl ThemeComponent {
    /// Number of supported component scopes.
    const COUNT: usize = Self::Footer as usize + 1;

    /// Find the parent region whose overrides are inherited per color.
    const fn parent(self) -> Option<Self> {
        match self {
            Self::Gutter => Some(Self::Editor),
            Self::ExplorerTree => Some(Self::Explorer),
            Self::Editor | Self::Explorer | Self::Footer => None,
        }
    }

    /// Map a table key `name` beneath `parent` to a supported region.
    fn from_key(parent: Option<Self>, name: &str) -> Option<Self> {
        match (parent, name) {
            (None, "editor") => Some(Self::Editor),
            (Some(Self::Editor), "gutter") => Some(Self::Gutter),
            (None, "explorer") => Some(Self::Explorer),
            (Some(Self::Explorer), "tree") => Some(Self::ExplorerTree),
            (None, "footer") => Some(Self::Footer),
            _ => None,
        }
    }
}

/// Optional color values indexed by retained role.
const COLOR_COUNT: usize = FIELDS.len() + SyntaxKind::ALL.len();
type Colors = [Option<u32>; COLOR_COUNT];

/// The global colors and sparse component overrides for one theme.
struct Palette {
    /// Global colors after taking any values omitted by the user from the bundled file.
    global: Colors,
    /// Explicit component colors, with unset values inherited only when queried.
    components: [Colors; ThemeComponent::COUNT],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            global: [None; COLOR_COUNT],
            components: [[None; COLOR_COUNT]; ThemeComponent::COUNT],
        }
    }
}

/// Stable retained role for this kind, preserving the original seven color keys.
pub(crate) fn syntax_color(kind: SyntaxKind) -> u32 {
    use SyntaxKind::*;
    match kind {
        Keyword => SYNTAX_KEYWORD,
        Type => SYNTAX_TYPE,
        Function => SYNTAX_FUNCTION,
        String => SYNTAX_STRING,
        Constant => SYNTAX_CONSTANT,
        Comment => SYNTAX_COMMENT,
        Attribute => SYNTAX_ATTRIBUTE,
        _ => ROLE_START + (((FIELDS.len() + kind as usize) as u32) << 8),
    }
}

/// Supply an initial palette role for independent language-specific groups.
fn fallback_color(kind: SyntaxKind) -> u32 {
    use SyntaxKind::*;
    if let Some(parent) = kind.parent() {
        return syntax_color(parent);
    }
    match kind {
        Module | Tag | SqlTable | MarkupTable => SYNTAX_TYPE,
        MarkupHeading | Operator => SYNTAX_KEYWORD,
        MarkupLink | MarkupUrl => SYNTAX_FUNCTION,
        MarkupCode | MarkupCodeBlock => SYNTAX_STRING,
        MarkupEmphasis | MarkupStrong | MarkupStrike | TagAttribute | CssSelector | CssProperty
        | ConfigKey | SqlParameter | Lifetime | Label => SYNTAX_ATTRIBUTE,
        MarkupQuote => SYNTAX_COMMENT,
        MarkupMarker | MarkupList => SYNTAX_CONSTANT,
        _ => TEXT,
    }
}

impl Palette {
    /// Resolve a sparse syntax key through its ancestors after applying component overrides.
    fn resolved(&self, component: Option<ThemeComponent>, mut index: usize) -> u32 {
        loop {
            if let Some(value) = component
                .and_then(|c| self.component_color(c, index))
                .or(self.global[index])
            {
                return value;
            }
            let kind = SyntaxKind::ALL[index - FIELDS.len()];
            index = role_index(fallback_color(kind)).expect("built-in fallback color");
        }
    }
    /// Find the nearest explicit override of `index` starting in `component`.
    fn component_color(&self, mut component: ThemeComponent, index: usize) -> Option<u32> {
        loop {
            if let Some(color) = self.components[component as usize][index] {
                return Some(color);
            }
            component = component.parent()?;
        }
    }
}

/// Both variants of the previous JSON file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyThemes {
    /// Custom values for the dark variant.
    #[serde(default)]
    dark: BTreeMap<String, String>,
    /// Custom values for the light variant.
    #[serde(default)]
    light: BTreeMap<String, String>,
}

/// Bundled palettes used for omitted roles and malformed or inaccessible files.
static DEFAULTS: LazyLock<[Palette; 2]> = LazyLock::new(|| {
    [DARK, LIGHT].map(|template| {
        let palette = parse(template, None).expect("validate bundled theme TOML");
        assert!(
            palette.global[..FIELDS.len()].iter().all(Option::is_some),
            "bundle every theme color"
        );
        palette
    })
});
/// The editable palettes loaded once for this process.
static CUSTOM: OnceLock<[Option<Palette>; 2]> = OnceLock::new();

/// Load the two independently editable palettes beside the executable.
pub(crate) fn load_for_editor() {
    CUSTOM.get_or_init(|| match nanobug::app_data::settings::path() {
        Ok(settings) => [false, true].map(|light| {
            let filename = if light {
                "theme-light.toml"
            } else {
                "theme-dark.toml"
            };
            let path = settings.with_file_name(filename);
            match load(&path, light) {
                Ok(palette) => Some(palette),
                Err(error) => {
                    eprintln!("Theme {}: {error:#}", path.display());
                    None
                }
            }
        }),
        Err(error) => {
            eprintln!("{error:#}");
            [None, None]
        }
    });
}

/// Select the loaded variant by `light`, falling back only for the affected variant.
fn palette(light: bool) -> &'static Palette {
    let variant = usize::from(light);
    CUSTOM
        .get()
        .and_then(|palettes| palettes[variant].as_ref())
        .unwrap_or(&DEFAULTS[variant])
}

/// Decode a retained `color` role, excluding literal colors outside the reserved range.
fn role_index(color: u32) -> Option<usize> {
    color
        .checked_sub(ROLE_START)
        .filter(|offset| offset & 0xff == 0)
        .map(|offset| (offset >> 8) as usize)
        .filter(|&index| index < COLOR_COUNT)
}

/// Resolve `color` in the chosen `light` variant and optional `component`, preserving literal colors.
pub(super) fn resolve(light: bool, component: Option<ThemeComponent>, color: u32) -> u32 {
    let Some(index) = role_index(color) else {
        return color;
    };
    palette(light).resolved(component, index)
}

/// Return the configured foreground of a semantic modifier, which is transparent by default.
pub(super) fn modifier_foreground(
    light: bool,
    component: Option<ThemeComponent>,
    kind: SyntaxKind,
) -> Option<u32> {
    let index = role_index(syntax_color(kind))?;
    let palette = palette(light);
    component
        .and_then(|region| palette.component_color(region, index))
        .or(palette.global[index])
}

/// Find an explicit `color` override inherited by `component` in the chosen `light` variant.
pub(super) fn component_color(light: bool, component: ThemeComponent, color: u32) -> Option<u32> {
    palette(light).component_color(component, role_index(color)?)
}

/// Load `path` for the given `light` variant or atomically create it on first use.
fn load(path: &Path, light: bool) -> Result<Palette> {
    let defaults = &DEFAULTS[usize::from(light)];
    match fs::read_to_string(path) {
        Ok(document) => parse(&document, Some(defaults)),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let parent = path.parent().context("Theme file has no parent")?;
            let template = if light { LIGHT } else { DARK };
            let document = migrate(parent, template, light)?;
            let parsed = parse(&document, Some(defaults))?;
            fs::create_dir_all(parent)?;
            let mut staged = tempfile::NamedTempFile::new_in(parent)?;
            staged.write_all(document.as_bytes())?;
            staged.as_file().sync_all()?;
            match staged.persist_noclobber(path) {
                Ok(_) => Ok(parsed),
                Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                    parse(&fs::read_to_string(path)?, Some(defaults))
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Apply the legacy values in `parent` to `template` for the given `light` variant.
fn migrate(parent: &Path, template: &str, light: bool) -> Result<String> {
    let legacy_path = parent.join(LEGACY_FILE);
    let legacy = match fs::read_to_string(&legacy_path) {
        Ok(json) => json,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(template.into()),
        Err(error) => return Err(error).with_context(|| format!("Read {}", legacy_path.display())),
    };
    let themes: LegacyThemes = serde_json::from_str(legacy.trim_start_matches('\u{feff}'))
        .with_context(|| format!("Parse {}", legacy_path.display()))?;
    let values = if light { themes.light } else { themes.dark };
    if values.is_empty() {
        return Ok(template.into());
    }
    let mut document = template.parse::<toml_edit::DocumentMut>()?;
    for (key, value) in values {
        read_color(&key, &value).with_context(|| format!("Migrate {}", legacy_path.display()))?;
        // Substitute only the value so the template's inline explanation survives migration.
        if !document.contains_key(&key) {
            document[&key] = toml_edit::value(value);
            continue;
        }
        let old = document[&key]
            .as_value_mut()
            .context("Bundled color missing")?;
        let mut replacement = toml_edit::Value::from(value);
        *replacement.decor_mut() = old.decor().clone();
        *old = replacement;
    }
    Ok(document.to_string())
}

/// Parse a theme `document`, taking omitted global values from `defaults` when supplied.
fn parse(document: &str, defaults: Option<&Palette>) -> Result<Palette> {
    let mut table: toml::Table = toml::from_str(document.trim_start_matches('\u{feff}'))?;
    let mut palette = Palette {
        global: defaults.map_or([None; COLOR_COUNT], |value| value.global),
        ..Palette::default()
    };
    if let Some(components) = table.remove("components") {
        let components = components
            .as_table()
            .context("components must be a table")?;
        for (key, value) in components {
            parse_component(&mut palette, None, key, value)
                .with_context(|| format!("components.{key}"))?;
        }
    }
    for (key, value) in &table {
        let (index, color) = read_value(key, value)?;
        palette.global[index] = Some(color);
    }
    Ok(palette)
}

/// Populate `palette` with overrides from `value` at the supported `name` under `parent`.
fn parse_component(
    palette: &mut Palette,
    parent: Option<ThemeComponent>,
    name: &str,
    value: &toml::Value,
) -> Result<()> {
    let component = ThemeComponent::from_key(parent, name).context("Unknown theme component")?;
    let table = value.as_table().context("Component must be a table")?;
    for (key, value) in table {
        if value.is_table() {
            parse_component(palette, Some(component), key, value).with_context(|| key.clone())?;
        } else {
            let (index, color) = read_value(key, value)?;
            palette.components[component as usize][index] = Some(color);
        }
    }
    Ok(())
}

/// Read a TOML color `value` for role `key`, rejecting other TOML value types.
fn read_value(key: &str, value: &toml::Value) -> Result<(usize, u32)> {
    let value = value
        .as_str()
        .with_context(|| format!("{key} must be a color string"))?;
    read_color(key, value)
}

/// Validate configured `value` against the hexadecimal width of color role `key`.
fn read_color(key: &str, value: &str) -> Result<(usize, u32)> {
    let (index, digits) = if let Some((index, (_, digits))) =
        FIELDS.iter().enumerate().find(|(_, field)| field.0 == key)
    {
        (index, *digits)
    } else if let Some(kind) = SyntaxKind::ALL.iter().find(|kind| kind.theme_key() == key) {
        (
            role_index(syntax_color(*kind)).expect("syntax color role"),
            6,
        )
    } else {
        bail!("Unknown theme color {key}");
    };
    let hex = value.strip_prefix('#').unwrap_or_default();
    ensure!(
        hex.len() == digits && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid color {key}: expected #{digits} hexadecimal digits"
    );
    Ok((index, u32::from_str_radix(hex, 16)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Migrate each variant independently, retaining explanations and existing files.
    #[test]
    fn migrate_only_missing_theme_files() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let legacy = directory.path().join(LEGACY_FILE);
        let dark = directory.path().join("theme-dark.toml");
        let light = directory.path().join("theme-light.toml");
        let json = r##"{"dark":{"accent":"#ABCDEF"},"light":{"accent":"#111111"}}"##;
        let light_contents = "accent = \"#F01234\" # 保留已有注释\n";
        fs::write(&legacy, json)?;
        fs::write(&light, light_contents)?;
        assert_eq!(
            load(&dark, false)?.global[Role::ACCENT as usize],
            Some(0xabcdef)
        );
        assert_eq!(
            load(&light, true)?.global[Role::ACCENT as usize],
            Some(0xf01234)
        );
        let contents = fs::read_to_string(&dark)?;
        let bundled = DARK
            .lines()
            .find(|line| line.starts_with("accent "))
            .unwrap();
        assert!(contents.contains(&bundled.replace("#9df0d4", "#ABCDEF")));
        assert_eq!(fs::read_to_string(&light)?, light_contents);
        assert_eq!(fs::read_to_string(&legacy)?, json);
        fs::write(&dark, "unknown = 1\n")?;
        assert!(load(&dark, false).is_err());
        assert_eq!(fs::read_to_string(&dark)?, "unknown = 1\n");
        assert!(load(&light, true).is_ok());
        Ok(())
    }

    /// A theme written before detailed syntax roles still drives descendants in the editor component.
    #[test]
    fn old_and_sparse_themes_inherit_syntax_colors() -> Result<()> {
        let palette = parse(
            "syntax_constant = '#aabbcc'\nsyntax_float = '#010203'\n[components.editor]\nsyntax_function = '#ff9911'\n",
            Some(&DEFAULTS[0]),
        )?;
        let rgb = |kind| {
            palette.resolved(
                Some(ThemeComponent::Editor),
                role_index(syntax_color(kind)).unwrap(),
            )
        };
        assert_eq!(rgb(SyntaxKind::Number), 0xaabbcc);
        assert_eq!(rgb(SyntaxKind::Float), 0x010203);
        assert_eq!(rgb(SyntaxKind::Method), 0xff9911);
        Ok(())
    }

    /// Existing editor text overrides inherit each variant's dedicated wave colors on upgrade.
    #[test]
    fn existing_themes_inherit_diagnostic_underline_colors() -> Result<()> {
        let editor = Some(ThemeComponent::Editor);
        for defaults in DEFAULTS.iter() {
            let loaded = parse(
                "[components.editor]\nwarning_text = '#334455'\nerror_text = '#112233'\n",
                Some(defaults),
            )?;
            for role in [Role::WARNING_UNDERLINE, Role::ERROR_UNDERLINE] {
                let index = role as usize;
                assert_eq!(
                    loaded.resolved(editor, index),
                    defaults.resolved(editor, index)
                );
            }
            assert_eq!(
                loaded.resolved(editor, Role::WARNING_TEXT as usize),
                0x334455
            );
            assert_eq!(loaded.resolved(editor, Role::ERROR_TEXT as usize), 0x112233);
        }
        Ok(())
    }

    /// A child can override one field and inherit another from its parent or global theme.
    #[test]
    fn component_colors_inherit_per_role() -> Result<()> {
        let palette = parse(
            r##"
text = "#112233"
[components.editor]
text = "#445566"
background = "#44556677"
[components.editor.gutter]
text = "#778899"
"##,
            Some(&DEFAULTS[0]),
        )?;
        let editor = ThemeComponent::Editor;
        let gutter = ThemeComponent::Gutter;
        let text = Role::TEXT as usize;
        let background = Role::BACKGROUND as usize;
        assert_eq!(palette.component_color(gutter, text), Some(0x778899));
        assert_eq!(
            palette.component_color(gutter, background),
            Some(0x44556677)
        );
        assert_eq!(palette.component_color(editor, text), Some(0x445566));
        assert_eq!(
            palette.component_color(ThemeComponent::Explorer, text),
            None
        );
        assert_eq!(palette.global[text], Some(0x112233));
        assert_eq!(
            palette.global[Role::SELECTION as usize],
            DEFAULTS[0].global[Role::SELECTION as usize]
        );
        Ok(())
    }

    /// Invalid legacy data remains available for repair without creating a replacement theme.
    #[test]
    fn invalid_legacy_remains_available_for_retry() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let legacy = directory.path().join(LEGACY_FILE);
        let path = directory.path().join("theme-dark.toml");
        fs::write(&legacy, r##"{"dark":{"text":"#1"}}"##)?;
        assert!(load(&path, false).is_err());
        assert!(!path.exists());
        fs::write(&legacy, "{}")?;
        load(&path, false)?;
        assert_eq!(fs::read_to_string(&path)?, DARK);
        Ok(())
    }
}
