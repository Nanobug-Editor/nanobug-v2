//! Named Ex styles and links kept separately from the active GUI palette.

use super::{Action, ExMessageOutput, ExOrigin, Register, Vim};
use std::{collections::BTreeMap, fmt::Write as _};

pub(super) const NAMES: &[&str] = &["highlight"];

/// Text attributes shared by the Ex registry and the host renderer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HighlightAttributes {
    /// Use a heavier face.
    pub bold: bool,
    /// Use a slanted face.
    pub italic: bool,
    /// Exchange foreground and background.
    pub reverse: bool,
    /// Emphasize the text with the host's standout treatment.
    pub standout: bool,
    /// Draw a straight line under the text.
    pub underline: bool,
    /// Draw a curved line under the text.
    pub undercurl: bool,
    /// Draw two lines under the text.
    pub underdouble: bool,
    /// Draw a dotted line under the text.
    pub underdotted: bool,
    /// Draw a dashed line under the text.
    pub underdashed: bool,
    /// Draw a line through the text.
    pub strikethrough: bool,
    /// Draw a line above the text.
    pub overline: bool,
    /// Use reduced intensity.
    pub dim: bool,
    /// Hide the text.
    pub conceal: bool,
    /// Replace underlying attributes when combining styles.
    pub nocombine: bool,
}

/// Resolved GUI colors and attributes of one named highlight group.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HighlightStyle {
    /// Foreground as RRGGBB, or the underlying text color.
    pub foreground: Option<u32>,
    /// Background as RRGGBB, or the underlying background.
    pub background: Option<u32>,
    /// Decoration color as RRGGBB, or the effective foreground.
    pub special: Option<u32>,
    /// Percentage of the underlying background to show through.
    pub blend: Option<u8>,
    /// Text and decoration attributes.
    pub attributes: HighlightAttributes,
}

/// Resolve palette-relative colors at lookup so they follow later changes to Normal.
#[derive(Clone, Copy)]
pub(super) enum HighlightColor {
    None,
    Rgb(u32),
    Foreground,
    Background,
}

impl HighlightColor {
    /// Look up a color against the current normal style.
    fn resolve(self, normal: HighlightStyle) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Rgb(rgb) => Some(rgb),
            Self::Foreground => normal.foreground,
            Self::Background => normal.background,
        }
    }

    /// Render the command spelling used to query or reapply this color.
    fn display(self) -> String {
        match self {
            Self::None => "NONE".into(),
            Self::Rgb(rgb) => format!("#{rgb:06x}"),
            Self::Foreground => "fg".into(),
            Self::Background => "bg".into(),
        }
    }
}

/// One explicit field in a highlight group, paired with its command spelling.
#[derive(Clone)]
pub(super) enum Property {
    Color(HighlightColor),
    Gui(HighlightAttributes, String),
    Blend(u8),
    Terminal(String),
}

impl Property {
    /// Write a value suitable for listing the group's current fields.
    fn display(&self) -> String {
        match self {
            Self::Color(color) => color.display(),
            Self::Gui(_, value) | Self::Terminal(value) => value.clone(),
            Self::Blend(value) => value.to_string(),
        }
    }
}

/// A group's explicit settings and default link tracked independently of palette colors.
#[derive(Default)]
struct Group {
    name: String,
    fields: BTreeMap<String, Property>,
    link: Option<String>,
    default_link: Option<String>,
    cleared: bool,
    origin: Option<ExOrigin>,
}

impl Group {
    /// Tell whether this entry changes how the host draws an existing palette role.
    fn overrides_palette(&self) -> bool {
        self.cleared || self.link.is_some() || !self.fields.is_empty()
    }
}

/// Mutable highlighting shared by the workspace's Ex interpreters.
pub(super) struct Highlights {
    /// Current GUI palette, indexed by case-insensitive names with the display spelling retained.
    palette: BTreeMap<String, (&'static str, HighlightStyle)>,
    /// User-defined groups and changes to palette groups.
    groups: BTreeMap<String, Group>,
    /// Last script location that restored the palette.
    reset_origin: Option<ExOrigin>,
}

impl Default for Highlights {
    fn default() -> Self {
        let mut groups = Self {
            palette: BTreeMap::new(),
            groups: BTreeMap::new(),
            reset_origin: None,
        };
        // A standalone editor model needs the ordinary groups even without the native host.
        groups.set_palette([
            (
                "Normal",
                HighlightStyle {
                    foreground: Some(0xd3d8e5),
                    background: Some(0x20212b),
                    ..Default::default()
                },
            ),
            (
                "Search",
                HighlightStyle {
                    background: Some(0xd9b365),
                    ..Default::default()
                },
            ),
            (
                "ErrorMsg",
                HighlightStyle {
                    foreground: Some(0xe88989),
                    ..Default::default()
                },
            ),
            (
                "WarningMsg",
                HighlightStyle {
                    foreground: Some(0xf5c078),
                    ..Default::default()
                },
            ),
            (
                "Comment",
                HighlightStyle {
                    foreground: Some(0x929baa),
                    ..Default::default()
                },
            ),
        ]);
        groups
    }
}

impl Highlights {
    /// Replace theme-provided colors while preserving Ex specifications and links.
    pub(super) fn set_palette(
        &mut self,
        palette: impl IntoIterator<Item = (&'static str, HighlightStyle)>,
    ) {
        self.palette = palette
            .into_iter()
            .map(|(name, style)| (name.to_ascii_lowercase(), (name, style)))
            .collect();
    }

    /// List names whose stored spelling begins with `prefix`, ignoring ASCII case.
    pub(super) fn names(&self, prefix: &str) -> Vec<String> {
        let prefix = prefix.to_ascii_lowercase();
        self.palette
            .keys()
            .chain(self.groups.keys())
            .filter(|key| key.starts_with(&prefix))
            .map(|key| {
                self.name(key)
                    .expect("registered highlight group")
                    .to_owned()
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Look up the display name for a normalized group key.
    fn name(&self, key: &str) -> Option<&str> {
        self.palette
            .get(key)
            .map(|(name, _)| *name)
            .or_else(|| self.groups.get(key).map(|group| group.name.as_str()))
    }

    /// Return the resolved style for a known group, optionally requiring an Ex override.
    pub(super) fn style(&self, name: &str, only_override: bool) -> Option<HighlightStyle> {
        let key = name.to_ascii_lowercase();
        if only_override && !self.groups.get(&key).is_some_and(Group::overrides_palette) {
            return None;
        }
        self.name(&key)?;
        let normal = self.style_for("normal", self.base_style("normal"), false);
        let mut style = if key == "normal" {
            normal
        } else {
            self.style_for(&key, normal, true)
        };
        if only_override
            && matches!(key.as_str(), "normal" | "normalnc")
            && self
                .groups
                .get(&key)
                .is_some_and(|group| group.link.is_none() && !group.fields.contains_key("guibg"))
        {
            // Attribute-only changes must keep the native window background and its opacity.
            style.background = None;
        }
        Some(style)
    }

    /// Read the native palette for `key` before applying a command or link.
    fn base_style(&self, key: &str) -> HighlightStyle {
        self.palette
            .get(key)
            .map(|(_, style)| *style)
            .unwrap_or_default()
    }

    /// Follow group links to a concrete style and resolve any `fg` or `bg` references.
    fn style_for<'a>(
        &'a self,
        mut key: &'a str,
        normal: HighlightStyle,
        inherit_normal: bool,
    ) -> HighlightStyle {
        let mut remaining = self.groups.len();
        while let Some(link) = self.groups.get(key).and_then(|group| group.link.as_deref()) {
            if remaining == 0 {
                return HighlightStyle::default();
            }
            remaining -= 1;
            key = link;
        }
        if key == "normal" && inherit_normal {
            return normal;
        }
        let Some(group) = self.groups.get(key) else {
            return self.base_style(key);
        };
        let mut style = if group.cleared {
            HighlightStyle::default()
        } else {
            self.base_style(key)
        };
        for (key, value) in &group.fields {
            match (key.as_str(), value) {
                ("guifg", Property::Color(color)) => style.foreground = color.resolve(normal),
                ("guibg", Property::Color(color)) => style.background = color.resolve(normal),
                ("guisp", Property::Color(color)) => style.special = color.resolve(normal),
                ("gui", Property::Gui(attributes, _)) => style.attributes = *attributes,
                ("blend", Property::Blend(value)) => style.blend = Some(*value),
                _ => {}
            }
        }
        style
    }

    /// Create the group `name` on first use and retain its original spelling.
    fn group(&mut self, name: &str) -> &mut Group {
        self.groups
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| Group {
                name: name.into(),
                ..Default::default()
            })
    }

    /// Check whether the group already has a definition for `:highlight default`.
    fn is_defined(&self, key: &str) -> bool {
        self.has_settings(key)
            || self
                .groups
                .get(key)
                .is_some_and(|group| group.link.is_some())
    }

    /// Determine whether a group sets actual style fields independently of its link.
    fn has_settings(&self, key: &str) -> bool {
        let group = self.groups.get(key);
        group.is_some_and(|group| !group.fields.is_empty())
            || !group.is_some_and(|group| group.cleared)
                && self.base_style(key) != HighlightStyle::default()
    }

    /// Apply a parsed operation or return a listing for the requested group, with optional provenance.
    pub(super) fn apply(
        &mut self,
        command: HighlightCommand,
        origin: Option<ExOrigin>,
        verbose: bool,
    ) -> Result<Option<ExMessageOutput>, String> {
        match command {
            HighlightCommand::List => {
                let mut result = ExMessageOutput::default();
                for name in self.names("") {
                    let line = self.describe(&name.to_ascii_lowercase(), &name, verbose);
                    if !result.text.is_empty() {
                        result.text.push('\n');
                    }
                    result.append_range(&line, 0..line.text.len());
                }
                return Ok(Some(result));
            }
            HighlightCommand::Query(name) => {
                let key = name.to_ascii_lowercase();
                let display = self
                    .name(&key)
                    .ok_or_else(|| format!("Unknown highlight group: {name}"))?;
                return Ok(Some(self.describe(&key, display, verbose)));
            }
            HighlightCommand::Reset => {
                for (key, group) in &mut self.groups {
                    group.fields.clear();
                    group.link.clone_from(&group.default_link);
                    group.cleared = !self.palette.contains_key(key) && group.link.is_none();
                    group.origin.clone_from(&origin);
                }
                self.reset_origin = origin;
            }
            HighlightCommand::Clear(name) => {
                let group = self.group(&name);
                group.fields.clear();
                group.link.clone_from(&group.default_link);
                group.cleared = true;
                group.origin = origin;
            }
            HighlightCommand::Set {
                name,
                fields,
                default,
                clear,
            } => {
                if !default || !self.is_defined(&name.to_ascii_lowercase()) {
                    let group = self.group(&name);
                    group.link = None;
                    if clear {
                        group.cleared = true;
                        group.fields.clear();
                    }
                    group.fields.extend(fields);
                    group.origin = origin;
                }
            }
            HighlightCommand::Link {
                from,
                to,
                default,
                force,
            } => {
                let key = from.to_ascii_lowercase();
                let target = to.as_ref().map(|name| name.to_ascii_lowercase());
                if let Some(target) = target.as_deref() {
                    let mut next = Some(target);
                    for _ in 0..=self.groups.len() {
                        let Some(link) = next else { break };
                        if link == key {
                            return Err(format!("Highlight link would form a cycle: {from}"));
                        }
                        next = self
                            .groups
                            .get(link)
                            .and_then(|group| group.link.as_deref());
                    }
                    if next.is_some() {
                        return Err(format!("Highlight link would form a cycle: {from}"));
                    }
                }
                let blocked = !force
                    && target.is_some()
                    && if default {
                        self.is_defined(&key)
                    } else {
                        self.has_settings(&key)
                    };
                if let Some(name) = to.as_deref() {
                    self.group(name);
                }
                let group = self.group(&from);
                if default && (force || group.default_link.is_none()) {
                    group.default_link.clone_from(&target);
                }
                if !blocked {
                    group.link = target;
                    group.origin = origin;
                } else if !default && origin.is_none() {
                    return Err(format!("Highlight group already has settings: {from}"));
                }
            }
        }
        Ok(None)
    }

    /// Render a palette or user group, keeping links and explicit terminal settings visible.
    fn describe(&self, key: &str, name: &str, verbose: bool) -> ExMessageOutput {
        let group = self.groups.get(key);
        let mut output = ExMessageOutput::default();
        output.push(&format!("{name:<15} "), None);
        output.push("xxx", self.style(name, false));
        let text = &mut output.text;
        if let Some(link) = group.and_then(|group| group.link.as_deref()) {
            let _ = write!(text, " links to {}", self.name(link).unwrap_or(link));
        } else {
            let mut fields = BTreeMap::new();
            if !group.is_some_and(|group| group.cleared) {
                let base = self.base_style(key);
                for (name, color) in [
                    ("guifg", base.foreground),
                    ("guibg", base.background),
                    ("guisp", base.special),
                ] {
                    if let Some(color) = color {
                        fields.insert(name, format!("#{color:06x}"));
                    }
                }
            }
            if let Some(group) = group {
                fields.extend(
                    group
                        .fields
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.display())),
                );
            }
            if fields.is_empty() {
                text.push_str(" cleared");
            }
            for (field, value) in fields {
                let _ = write!(text, " {field}={value}");
            }
        }
        if verbose
            && let Some(origin) = group
                .and_then(|group| group.origin.as_ref())
                .or(self.reset_origin.as_ref())
        {
            origin.append_to(text);
        }
        output
    }
}

/// One command against the workspace's named highlight definitions.
pub(super) enum HighlightCommand {
    List,
    Query(String),
    Reset,
    Clear(String),
    Set {
        name: String,
        fields: BTreeMap<String, Property>,
        default: bool,
        clear: bool,
    },
    Link {
        from: String,
        to: Option<String>,
        default: bool,
        force: bool,
    },
}

impl Vim {
    /// Apply `:highlight` and route the listing through the existing Ex output channel.
    pub(super) fn ex_highlight_command(
        &mut self,
        text: &str,
        register: &Register,
    ) -> Option<Action> {
        let mut statements = super::script::statements(text);
        let first = statements.next()?;
        let (word, arguments) = first.split_once(char::is_whitespace).unwrap_or((first, ""));
        let name = word.trim_end_matches('!');
        if !is_highlight(name) {
            return None;
        }
        if statements.next().is_some() {
            return Some(Action::ExPipeline(text.into()));
        }
        let result = parse(arguments, word.ends_with('!'))
            .and_then(|command| register.input_maps.apply_highlight(command));
        Some(match result {
            Ok(Some(list)) if arguments.trim().is_empty() => Action::ShowStyledListing(list),
            Ok(Some(line)) => {
                self.publish_styled_ex_message(line);
                Action::None
            }
            Ok(None) => Action::None,
            Err(error) => {
                self.script_error(&error);
                Action::None
            }
        })
    }
}

/// Recognize the native `hi[ghlight]` spelling.
fn is_highlight(name: &str) -> bool {
    name.len() >= 2 && "highlight".starts_with(name)
}

/// Complete a query, group definition, clear, or the source and target of a link.
pub(super) fn complete_argument<'a>(
    name: &str,
    arguments: &'a str,
    register: &Register,
) -> Option<(&'a str, Vec<String>)> {
    if !is_highlight(name.trim_end_matches('!')) {
        return None;
    }
    let prefix = arguments
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or_default();
    let words = arguments[..arguments.len() - prefix.len()]
        .split_whitespace()
        .collect::<Vec<_>>();
    let mut candidates = register.input_maps.highlight_groups(prefix);
    if words.is_empty() || words == ["default"] {
        candidates.extend(
            ["clear", "default", "link"]
                .into_iter()
                .filter(|word| word.starts_with(prefix))
                .map(str::to_owned),
        );
    }
    candidates.sort();
    candidates.dedup();
    Some((prefix, candidates))
}

/// Parse a full command before mutating a group, so an invalid field leaves the previous style intact.
fn parse(arguments: &str, force: bool) -> Result<HighlightCommand, String> {
    let words = highlight_words(arguments)?;
    let (default, words) = if words
        .first()
        .is_some_and(|word| "default".starts_with(word.as_str()))
    {
        (true, &words[1..])
    } else {
        (false, words.as_slice())
    };
    let command = match words {
        [] if !default => HighlightCommand::List,
        [clear] if is_keyword(clear, "clear") && !default => HighlightCommand::Reset,
        [clear, name] if is_keyword(clear, "clear") && !default => {
            HighlightCommand::Clear(group_name(name)?)
        }
        [link, from, to] if is_keyword(link, "link") => HighlightCommand::Link {
            from: group_name(from)?,
            to: (!to.eq_ignore_ascii_case("NONE"))
                .then(|| group_name(to))
                .transpose()?,
            default,
            force,
        },
        [name] if !is_keyword(name, "link") && !is_keyword(name, "clear") => {
            HighlightCommand::Query(group_name(name)?)
        }
        [name, none] if none.eq_ignore_ascii_case("NONE") && !default => {
            HighlightCommand::Clear(group_name(name)?)
        }
        [name, fields @ ..]
            if !fields.is_empty() && !is_keyword(name, "clear") && !is_keyword(name, "link") =>
        {
            let (fields, clear) = parse_fields(fields)?;
            HighlightCommand::Set {
                name: group_name(name)?,
                fields,
                default,
                clear,
            }
        }
        _ => return Err(format!("Invalid highlight command: {arguments}")),
    };
    Ok(command)
}

/// Accept optional spaces around `=` and let a `NONE` argument clear earlier fields.
fn parse_fields(words: &[String]) -> Result<(BTreeMap<String, Property>, bool), String> {
    let mut fields = BTreeMap::new();
    let mut clear = false;
    let mut words = words.iter().peekable();
    while let Some(word) = words.next() {
        if word.eq_ignore_ascii_case("NONE") {
            clear = true;
            fields.clear();
            continue;
        }
        let mut field = word.clone();
        if !field.contains('=') && words.peek().is_some_and(|word| word.starts_with('=')) {
            field.push_str(words.next().expect("peeked field"));
        }
        if field.ends_with('=')
            && let Some(value) = words.next()
        {
            field.push_str(value);
        }
        let (key, value) = parse_property(&field)?;
        fields.insert(key, value);
    }
    Ok((fields, clear))
}

/// Match an Ex subcommand using its native prefix.
fn is_keyword(word: &str, keyword: &str) -> bool {
    !word.is_empty() && keyword.starts_with(word)
}

/// Keep the supported ASCII group alphabet, including Tree-sitter-style names.
fn group_name(name: &str) -> Result<String, String> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@.".contains(&byte))
    {
        Ok(name.into())
    } else {
        Err(format!("Invalid highlight group: {name}"))
    }
}

/// Split fields on whitespace, preserving quoted font names and multiword color names.
fn highlight_words(arguments: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for ch in arguments.chars() {
        if ch == '\'' {
            quoted = !quoted;
        } else if ch == '"' && !quoted && word.is_empty() {
            break;
        } else if ch.is_whitespace() && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if quoted {
        return Err("Unterminated highlight value".into());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

/// Validate one highlight field and keep the terminal-only settings available in listings.
fn parse_property(field: &str) -> Result<(String, Property), String> {
    let (key, value) = field
        .split_once('=')
        .ok_or_else(|| format!("Expected a highlight setting: {field}"))?;
    let key = key.to_ascii_lowercase();
    if value.is_empty() {
        return Err(format!("Missing highlight value: {field}"));
    }
    let property = match key.as_str() {
        "guifg" | "guibg" | "guisp" => Property::Color(parse_color(value)?),
        "gui" => Property::Gui(parse_attributes(value)?, value.to_owned()),
        "cterm" | "term" => {
            parse_attributes(value)?;
            Property::Terminal(value.into())
        }
        "ctermfg" | "ctermbg" | "ctermul" => {
            if value.parse::<u8>().is_err() {
                parse_color(value)?;
            }
            Property::Terminal(value.into())
        }
        "font" | "start" | "stop" => Property::Terminal(value.into()),
        "blend" => Property::Blend(
            value
                .parse::<u8>()
                .ok()
                .filter(|&n| n <= 100)
                .ok_or_else(|| format!("Invalid highlight blend: {value}"))?,
        ),
        _ => return Err(format!("Unknown highlight setting: {key}")),
    };
    Ok((key, property))
}

/// Recognize the named attributes used by both the GUI and terminal styles.
fn parse_attributes(value: &str) -> Result<HighlightAttributes, String> {
    let mut attributes = HighlightAttributes::default();
    for name in value.split(',') {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "underline" | "undercurl" | "underdouble" | "underdotted" | "underdashed"
        ) {
            attributes.underline = false;
            attributes.undercurl = false;
            attributes.underdouble = false;
            attributes.underdotted = false;
            attributes.underdashed = false;
        }
        match name.to_ascii_lowercase().as_str() {
            "none" if value.eq_ignore_ascii_case("none") => {}
            "bold" => attributes.bold = true,
            "italic" => attributes.italic = true,
            "reverse" | "inverse" => attributes.reverse = true,
            "standout" => attributes.standout = true,
            "underline" => attributes.underline = true,
            "undercurl" => attributes.undercurl = true,
            "underdouble" => attributes.underdouble = true,
            "underdotted" => attributes.underdotted = true,
            "underdashed" => attributes.underdashed = true,
            "strikethrough" => attributes.strikethrough = true,
            "overline" => attributes.overline = true,
            "dim" => attributes.dim = true,
            "conceal" => attributes.conceal = true,
            "nocombine" => attributes.nocombine = true,
            "altfont" | "blink" => {}
            _ => return Err(format!("Unknown highlight attribute: {name}")),
        }
    }
    Ok(attributes)
}

/// Parse a GUI RGB value, common color name, or reference to Normal.
fn parse_color(value: &str) -> Result<HighlightColor, String> {
    let color = value.to_ascii_lowercase().replace([' ', '_'], "");
    let rgb = match color.as_str() {
        "none" => return Ok(HighlightColor::None),
        "fg" | "foreground" => return Ok(HighlightColor::Foreground),
        "bg" | "background" => return Ok(HighlightColor::Background),
        "black" => 0x000000,
        "white" => 0xffffff,
        "red" => 0xff0000,
        "lightred" => 0xff8b8b,
        "darkred" => 0x8b0000,
        "green" => 0x00ff00,
        "lightgreen" => 0x90ee90,
        "darkgreen" => 0x006400,
        "seagreen" => 0x2e8b57,
        "blue" => 0x0000ff,
        "lightblue" => 0xadd8e6,
        "darkblue" => 0x00008b,
        "slateblue" => 0x6a5acd,
        "cyan" | "aqua" => 0x00ffff,
        "lightcyan" => 0xe0ffff,
        "darkcyan" => 0x008b8b,
        "magenta" | "fuchsia" => 0xff00ff,
        "lightmagenta" => 0xff8bff,
        "darkmagenta" => 0x8b008b,
        "yellow" => 0xffff00,
        "lightyellow" => 0xffffe0,
        "darkyellow" => 0xbbbb00,
        "brown" => 0xa52a2a,
        "gray" | "grey" => 0xbebebe,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "orange" => 0xffa500,
        "purple" => 0xa020f0,
        "violet" => 0xee82ee,
        "salmonpink" => 0xff91a4,
        "salmon" => 0xfa8072,
        _ => color
            .strip_prefix('#')
            .filter(|hex| hex.len() == 6)
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .ok_or_else(|| format!("Unknown highlight color: {value}"))?,
    };
    Ok(HighlightColor::Rgb(rgb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Execute one non-listing command and include its text in any test failure.
    fn run(vim: &mut Vim, register: &mut Register, command: &str) {
        assert_eq!(vim.execute_ex_line(command, register), Action::None);
        assert!(!vim.ex_failed(), "{command}: {}", vim.message);
    }

    /// Preserve independent fields, enforce explicit links, and restore the selected palette.
    #[test]
    fn highlight_groups_merge_link_and_clear() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        let mappings = register.input_maps.clone();
        let original = mappings.highlight_style("Comment").unwrap();
        run(
            &mut vim,
            &mut register,
            "hi Comment gui=italic guifg=#123456",
        );
        run(
            &mut vim,
            &mut register,
            "hi cOMMENT guibg = #ffddee ctermfg=4",
        );
        run(&mut vim, &mut register, "hi default Comment guifg=#ffffff");
        let comment = mappings.highlight_override("comment").unwrap();
        assert_eq!(comment.foreground, Some(0x123456));
        assert_eq!(comment.background, Some(0xffddee));
        assert!(comment.attributes.italic);
        run(&mut vim, &mut register, "highlight comment");
        assert!(vim.message.contains("ctermfg=4"));
        assert!(vim.message.contains("guifg=#123456"));
        let sample = vim.message.find("xxx").unwrap();
        assert_eq!(vim.ex_message_spans()[0].range, sample..sample + 3);
        assert_eq!(vim.ex_message_spans()[0].style, comment);

        run(&mut vim, &mut register, "hi link Task Comment");
        assert_eq!(mappings.highlight_style("tAsK"), Some(comment));
        vim.execute_ex_line("hi link Comment Normal", &mut register);
        assert!(vim.ex_failed());
        assert_eq!(mappings.highlight_style("Comment"), Some(comment));
        run(&mut vim, &mut register, "hi! link Comment Normal");
        assert_eq!(
            mappings.highlight_style("Task"),
            mappings.highlight_style("Normal")
        );
        run(&mut vim, &mut register, "hi link Comment NONE");
        assert_eq!(mappings.highlight_style("Comment"), Some(comment));

        run(&mut vim, &mut register, "hi clear Comment");
        assert_eq!(
            mappings.highlight_style("Comment"),
            Some(HighlightStyle::default())
        );
        run(&mut vim, &mut register, "hi clear");
        assert_eq!(mappings.highlight_style("Comment"), Some(original));
        assert_eq!(mappings.highlight_override("Comment"), None);
        assert_eq!(
            mappings.highlight_style("Task"),
            Some(HighlightStyle::default())
        );
    }

    /// Keep relative colors dynamic and remember default links and assignment locations.
    #[test]
    fn highlight_palette_switch_and_verbose_origin() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        let mappings = register.input_maps.clone();
        let script =
            crate::vim::ScriptProgram::compile(Vec::new(), Some("theme.vim".into())).unwrap();
        {
            let _origin = mappings.enter_ex_origin(script.ex_origin(12));
            run(
                &mut vim,
                &mut register,
                "hi Relative guifg=fg guibg=bg guisp='salmon pink' gui=undercurl",
            );
            run(&mut vim, &mut register, "hi default link Task Relative");
        }
        run(&mut vim, &mut register, "verbose highlight Relative");
        assert!(vim.message.contains("theme.vim line 12"), "{}", vim.message);
        let initial = mappings.highlight_style("Task").unwrap();
        assert!(initial.attributes.undercurl);
        assert_eq!(initial.special, Some(0xff91a4));
        mappings.set_highlight_palette([(
            "Normal",
            HighlightStyle {
                foreground: Some(0x0a0b0c),
                background: Some(0x112233),
                ..Default::default()
            },
        )]);
        let switched = mappings.highlight_style("Task").unwrap();
        assert_eq!(switched.foreground, Some(0x0a0b0c));
        assert_eq!(switched.background, Some(0x112233));
        vim.execute_ex_line("hi! link Relative Task", &mut register);
        assert!(vim.ex_failed());
        assert_eq!(mappings.highlight_style("Task"), Some(switched));
        run(&mut vim, &mut register, "hi! default link Task Normal");
        run(&mut vim, &mut register, "hi clear");
        assert_eq!(
            mappings.highlight_style("Task"),
            mappings.highlight_style("Normal")
        );
        assert_eq!(mappings.highlight_groups("tas"), vec!["Task"]);
    }

    /// Use a Normal override only for the fields needed by the native editor's overlay.
    #[test]
    fn highlight_normal_preserves_material_until_background_is_set() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        let mappings = register.input_maps.clone();
        let base = mappings.highlight_style("Normal").unwrap();
        run(&mut vim, &mut register, "hi Normal gui=bold");
        assert_eq!(
            mappings.highlight_style("Normal").unwrap().background,
            base.background
        );
        assert_eq!(
            mappings.highlight_override("Normal").unwrap().background,
            None
        );
        run(&mut vim, &mut register, "hi Normal guifg=bg guibg=fg");
        assert_eq!(
            mappings.highlight_style("Normal").unwrap().foreground,
            base.background
        );
        assert_eq!(
            mappings.highlight_override("Normal").unwrap().background,
            base.foreground
        );
        run(&mut vim, &mut register, "hi link Swapped Normal");
        assert_eq!(
            mappings.highlight_style("Swapped"),
            mappings.highlight_style("Normal")
        );
    }
}
