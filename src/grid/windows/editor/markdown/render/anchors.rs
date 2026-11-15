//! Index named Markdown destinations at their laid-out preview positions.

use super::Inline;
use gpui::Pixels;
use percent_encoding::percent_decode_str;
use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};

/// Characters outside the heading identifier's letter, mark, number and separator set.
static HEADING_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{M}\p{N}_-]").expect("heading punctuation regex"));

/// Positions of clickable destinations in one preview layout.
#[derive(Default)]
pub(super) struct Anchors {
    /// First vertical position for each visible HTML or generated ID.
    positions: HashMap<String, Pixels>,
    /// Most recent suffix per Markdown heading ID, including generated numbered IDs.
    heading_suffixes: HashMap<String, usize>,
}

impl Anchors {
    /// Record an explicit HTML `id` at the first position `top` that declares it.
    pub(super) fn insert(&mut self, id: String, top: Pixels) {
        self.positions.entry(id).or_insert(top);
    }

    /// Record a heading at `top`, either under its explicit `id` or a unique title-derived ID.
    pub(super) fn heading(&mut self, inlines: &[Inline], id: Option<&str>, top: Pixels) {
        if let Some(id) = id {
            self.insert(id.into(), top);
            return;
        }

        let base = heading_id(inlines);
        if base.is_empty() {
            return;
        }
        let mut suffix = self.heading_suffixes.get(&base).copied().unwrap_or(0);
        let mut id = base.clone();
        while self.heading_suffixes.contains_key(&id) {
            suffix += 1;
            id = format!("{base}-{suffix}");
        }
        if suffix != 0 {
            self.heading_suffixes.insert(base, suffix);
        }
        self.heading_suffixes.insert(id.clone(), 0);
        self.insert(id, top);
    }

    /// Resolve a percent-encoded URL `fragment` to a logical preview position.
    pub(super) fn top(&self, fragment: &str) -> Option<Pixels> {
        let id = percent_decode_str(fragment).decode_utf8().ok()?;
        self.positions.get(id.as_ref()).copied()
    }
}

/// Derive a heading ID from visible `inlines`, retaining letters, numbers and existing separators.
fn heading_id(inlines: &[Inline]) -> String {
    let mut title = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { content, .. } => title.push_str(content),
            Inline::Break => title.push(' '),
            _ => {}
        }
    }
    let title = title.trim().to_lowercase().replace(' ', "-");
    HEADING_PUNCTUATION.replace_all(&title, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::windows::editor::markdown::parser;
    use gpui::px;

    /// Keep generated headings unique and resolve UTF-8 fragments and case-sensitive HTML IDs.
    #[test]
    fn markdown_anchors_resolve_named_and_repeated_headings() {
        let headings = parser::parse(concat!(
            "# **Release** 信息!\n\n",
            "## Release 信息-1\n\n",
            "# Release 信息!\n\n",
            "<h2 id=\"ExactID\">HTML heading</h2>",
        ));
        let mut anchors = Anchors::default();
        for (index, heading) in headings.blocks.iter().enumerate() {
            let parser::Block::Heading { inlines, id, .. } = &heading.block else {
                panic!("expected heading {index}");
            };
            anchors.heading(inlines, id.as_deref(), px(10. + index as f32 * 30.));
        }
        assert_eq!(anchors.top("release-%E4%BF%A1%E6%81%AF"), Some(px(10.)));
        assert_eq!(anchors.top("release-信息-1"), Some(px(40.)));
        assert_eq!(anchors.top("release-信息-2"), Some(px(70.)));
        assert_eq!(anchors.top("ExactID"), Some(px(100.)));
        assert_eq!(anchors.top("exactid"), None);
        assert_eq!(anchors.top("%FF"), None);
    }
}
