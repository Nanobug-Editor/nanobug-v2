//! Localized character buttons whose drawing and input share one access key.

use super::render::LabelStyle;

/// A translated caption split around its marked ASCII access key.
#[derive(Clone, Copy)]
pub(in crate::grid) struct Mnemonic {
    /// Ordinary text before the marked letter.
    prefix: &'static str,
    /// The underlined access key, or an empty string for a plain caption.
    letter: &'static str,
    /// Ordinary text after the letter.
    suffix: &'static str,
}

impl Mnemonic {
    /// Split `label` at its single `&` marker before an ASCII letter.
    pub(in crate::grid) fn parse(label: &'static str) -> Self {
        if let Some((prefix, tail)) = label.split_once('&')
            && let Some(letter) = tail
                .get(..1)
                .filter(|s| s.as_bytes()[0].is_ascii_alphabetic())
        {
            Self {
                prefix,
                letter,
                suffix: &tail[1..],
            }
        } else {
            Self {
                prefix: label,
                letter: "",
                suffix: "",
            }
        }
    }

    /// Return the bracketed caption and underline only its access key.
    pub(in crate::grid) fn spans(self) -> [(&'static str, LabelStyle); 5] {
        [
            ("[", LabelStyle::Text),
            (self.prefix, LabelStyle::Text),
            (self.letter, LabelStyle::Key),
            (self.suffix, LabelStyle::Text),
            ("]", LabelStyle::Text),
        ]
    }

    /// Match the marked letter against the native `key` independently of case.
    pub(in crate::grid) fn matches(self, key: &str) -> bool {
        !self.letter.is_empty() && self.letter.eq_ignore_ascii_case(key)
    }
}
