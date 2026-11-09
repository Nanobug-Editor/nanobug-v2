//! Embedded application messages shared by the editing models and native shell.

use std::{
    collections::HashMap,
    fmt::{Display, Write},
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// The language used by all projects in this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    /// English, including fallback for untranslated messages.
    English,
    /// Simplified Chinese (`zh-CN`).
    SimplifiedChinese,
}

/// A TOML catalog grouped by UI area, with string values addressed as `section.name`.
type Catalog = HashMap<String, HashMap<String, String>>;

/// English source messages and fallback catalog.
static ENGLISH: LazyLock<Catalog> = LazyLock::new(|| {
    toml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/i18n/en.toml"
    )))
    .expect("read embedded English messages")
});

/// Simplified Chinese translations, loaded only when selected.
static CHINESE: LazyLock<Catalog> = LazyLock::new(|| {
    toml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/i18n/zh-CN.toml"
    )))
    .expect("read embedded Simplified Chinese messages")
});

/// Process-wide Chinese flag, initialized from the optional startup override.
static USE_CHINESE: LazyLock<AtomicBool> = LazyLock::new(|| {
    let initial = Locale::from_tag(std::env::var("NANOBUG_LANG").ok().as_deref());
    AtomicBool::new(initial == Locale::SimplifiedChinese)
});

impl Locale {
    /// Resolve an optional startup language `tag`, defaulting to English.
    fn from_tag(tag: Option<&str>) -> Self {
        match tag {
            Some(tag) if tag.trim().eq_ignore_ascii_case("zh-CN") => Self::SimplifiedChinese,
            _ => Self::English,
        }
    }
}

/// Return the active locale shared by all projects.
pub fn locale() -> Locale {
    if USE_CHINESE.load(Ordering::Relaxed) {
        Locale::SimplifiedChinese
    } else {
        Locale::English
    }
}

/// Select `locale` for this process. Return whether the language changed.
pub fn set_locale(locale: Locale) -> bool {
    let chinese = locale == Locale::SimplifiedChinese;
    USE_CHINESE.swap(chinese, Ordering::Relaxed) != chinese
}

/// Resolve a `section.name` message `key`, falling back to English and then the key itself.
pub fn text(key: &'static str) -> &'static str {
    let Some((section, name)) = key.split_once('.') else {
        return key;
    };
    let lookup = |catalog: &'static Catalog| catalog.get(section)?.get(name).map(String::as_str);
    let translated = match locale() {
        Locale::English => None,
        Locale::SimplifiedChinese => lookup(&CHINESE),
    };
    translated.or_else(|| lookup(&ENGLISH)).unwrap_or(key)
}

/// Render the message at `key`, replacing named placeholders with their values from `args`.
/// Values are written verbatim, so braces in paths, errors and user input stay literal.
pub fn format(key: &'static str, args: &[(&str, &dyn Display)]) -> String {
    let mut remaining = text(key);
    let mut output = String::with_capacity(remaining.len());
    while let Some((prefix, placeholder)) = remaining.split_once('{') {
        let Some((name, tail)) = placeholder.split_once('}') else {
            break;
        };
        output.push_str(prefix);
        if let Some((_, value)) = args.iter().find(|(arg, _)| *arg == name) {
            write!(output, "{value}").expect("write localized message");
        } else {
            output.push('{');
            output.push_str(name);
            output.push('}');
        }
        remaining = tail;
    }
    output.push_str(remaining);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Collect the named placeholders used by a translation `message`.
    fn placeholders(message: &str) -> BTreeSet<&str> {
        message
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect()
    }

    /// Every shipped message can be rendered with the same inputs in both languages.
    #[test]
    fn catalogs_share_keys_and_placeholders() {
        assert_eq!(ENGLISH.len(), CHINESE.len());
        for (section, english) in ENGLISH.iter() {
            let chinese = &CHINESE[section];
            assert_eq!(english.len(), chinese.len(), "{section}");
            for (key, message) in english {
                assert_eq!(
                    placeholders(message),
                    placeholders(&chinese[key]),
                    "{section}.{key}"
                );
            }
        }
    }

    /// Startup defaults to English, and interpolated paths remain literal.
    #[test]
    fn default_locale_and_literal_arguments() {
        assert_eq!(Locale::from_tag(None), Locale::English);
        assert_eq!(Locale::from_tag(Some("en")), Locale::English);
        assert_eq!(Locale::from_tag(Some("unknown")), Locale::English);
        assert_eq!(Locale::from_tag(Some("zh-CN")), Locale::SimplifiedChinese);
        let message = format(
            "workspace.open_project_failed",
            &[("path", &"a{error}"), ("error", &"permission denied")],
        );
        assert!(message.contains("a{error}"));
        assert!(message.contains("permission denied"));
    }
}
