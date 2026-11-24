//! Localized character buttons whose drawing and input share one access key.

use super::text::{LabelStyle, TextSpacing};

/// A translated caption split around its marked ASCII access key.
#[derive(Clone, Copy)]
pub(crate) struct Mnemonic {
    /// Ordinary text before the marked letter.
    prefix: &'static str,
    /// Optional leading glyph drawn with the symbol font.
    symbol: &'static str,
    /// The underlined access key, or an empty string for a plain caption.
    letter: &'static str,
    /// Ordinary text after the letter.
    suffix: &'static str,
    /// Native key name used to activate the displayed access key.
    key: &'static str,
}

impl Mnemonic {
    /// Split `label` at its single `&` marker before an ASCII letter.
    pub(crate) fn parse(label: &'static str) -> Self {
        if let Some((prefix, tail)) = label.split_once('&')
            && let Some(letter) = tail
                .get(..1)
                .filter(|s| s.as_bytes()[0].is_ascii_alphabetic())
        {
            Self {
                symbol: "",
                prefix,
                letter,
                suffix: &tail[1..],
                key: letter,
            }
        } else {
            Self {
                symbol: "",
                prefix: label,
                letter: "",
                suffix: "",
                key: "",
            }
        }
    }

    /// Show a physical key with `prefix` and underlined `legend`, matched by its native `key`.
    pub(crate) fn named_key(prefix: &'static str, legend: &'static str, key: &'static str) -> Self {
        Self {
            symbol: "",
            prefix,
            letter: legend,
            suffix: "",
            key,
        }
    }

    /// Show a symbol followed by a space and an underlined physical key.
    pub(crate) fn symbol_key(
        symbol: &'static str,
        legend: &'static str,
        key: &'static str,
    ) -> Self {
        Self {
            symbol,
            ..Self::named_key(" ", legend, key)
        }
    }

    /// Measure the same bracketed spans used for drawing in grid `spacing`.
    pub(crate) fn columns(self, spacing: &TextSpacing) -> usize {
        self.spans()
            .into_iter()
            .map(|(text, _)| spacing.columns(text))
            .sum()
    }

    /// Return the bracketed caption and underline only its access key.
    pub(crate) fn spans(self) -> [(&'static str, LabelStyle); 6] {
        [
            ("[", LabelStyle::Text),
            (self.symbol, LabelStyle::Symbol),
            (self.prefix, LabelStyle::Text),
            (self.letter, LabelStyle::Key),
            (self.suffix, LabelStyle::Text),
            ("]", LabelStyle::Text),
        ]
    }

    /// Match the marked letter against the native `key` independently of case.
    pub(crate) fn matches(self, key: &str) -> bool {
        !self.key.is_empty() && self.key.eq_ignore_ascii_case(key)
    }
}
