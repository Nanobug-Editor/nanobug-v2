//! Embedded application messages shared by the editing models and native shell.

use crate::app_data::settings;
use anyhow::Result;
use std::{
    collections::HashMap,
    fmt::{Display, Write},
    path::Path,
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

/// Process-wide Chinese flag, initialized from the startup override or saved preference.
static USE_CHINESE: LazyLock<AtomicBool> = LazyLock::new(|| {
    let override_tag = std::env::var("NANOBUG_LANG").ok();
    let path = settings::path().ok();
    let initial = startup_locale(override_tag.as_deref(), path.as_deref());
    AtomicBool::new(initial == Locale::SimplifiedChinese)
});

impl Locale {
    /// Resolve a supported language `tag`, ignoring case and surrounding whitespace.
    fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.trim();
        if tag.eq_ignore_ascii_case("zh-CN") {
            Some(Self::SimplifiedChinese)
        } else if tag.eq_ignore_ascii_case("en") {
            Some(Self::English)
        } else {
            None
        }
    }

    /// Return the canonical TOML language tag for this locale.
    pub fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }
}

/// Use `override_tag` for startup when supported, then the preference at `path`, then English.
fn startup_locale(override_tag: Option<&str>, path: Option<&Path>) -> Locale {
    if let Some(locale) = override_tag.and_then(Locale::from_tag) {
        return locale;
    }
    path.and_then(|path| settings::read(path).ok())
        .as_ref()
        .and_then(|settings| settings.get("ui")?.get("language")?.as_str())
        .and_then(Locale::from_tag)
        .unwrap_or(Locale::English)
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

/// Save an explicitly selected `locale` in the running editor's settings for future launches.
pub fn save_locale(locale: Locale) -> Result<()> {
    save_locale_to(&settings::path()?, locale)
}

/// Update `ui.language` at `path` to `locale`, keeping the other settings and comments.
pub fn save_locale_to(path: &Path, locale: Locale) -> Result<()> {
    settings::save_ui_value(path, "language", locale.tag().into())
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
        assert_eq!(startup_locale(None, None), Locale::English);
        assert_eq!(startup_locale(Some("unknown"), None), Locale::English);
        assert_eq!(startup_locale(Some("en"), None), Locale::English);
        assert_eq!(
            startup_locale(Some(" ZH-cn "), None),
            Locale::SimplifiedChinese
        );
        let message = format(
            "workspace.open_project_failed",
            &[("path", &"a{error}"), ("error", &"permission denied")],
        );
        assert!(message.contains("a{error}"));
        assert!(message.contains("permission denied"));
    }

    /// Confirm a startup override is read-only and an explicit save updates just the language.
    #[test]
    fn language_preference_survives_startup_override() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("data/settings.toml");
        assert_eq!(
            startup_locale(Some("zh-CN"), Some(&path)),
            Locale::SimplifiedChinese
        );
        assert!(!path.exists());
        save_locale_to(&path, Locale::SimplifiedChinese)?;
        let saved = std::fs::read_to_string(&path)?;
        assert_eq!(saved, "[ui]\nlanguage = \"zh-CN\"\n");
        assert_eq!(startup_locale(Some("en"), Some(&path)), Locale::English);
        assert_eq!(std::fs::read_to_string(&path)?, saved);
        assert_eq!(startup_locale(None, Some(&path)), Locale::SimplifiedChinese);
        assert_eq!(
            startup_locale(Some("unknown"), Some(&path)),
            Locale::SimplifiedChinese
        );

        let with_comment = "[ui]\nlanguage = 'zh-CN' # startup language\nzoom = 125\n\n[editor]\nline_numbers = true\n";
        std::fs::write(&path, with_comment)?;
        save_locale_to(&path, Locale::English)?;
        assert_eq!(
            std::fs::read_to_string(&path)?,
            with_comment.replace("'zh-CN'", "\"en\"")
        );
        assert_eq!(startup_locale(None, Some(&path)), Locale::English);
        Ok(())
    }

    /// Keep a malformed settings file available for repair when an explicit save fails.
    #[test]
    fn language_save_preserves_unreadable_settings() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.toml");
        let invalid = "[ui\nlanguage = 'zh-CN'\n";
        std::fs::write(&path, invalid)?;
        assert_eq!(startup_locale(None, Some(&path)), Locale::English);
        assert!(save_locale_to(&path, Locale::English).is_err());
        assert_eq!(std::fs::read_to_string(path)?, invalid);
        Ok(())
    }
}
