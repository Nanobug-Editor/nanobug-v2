//! Read-only configuration previews with the same measured text as the manager list.

use super::Detail;
use crate::ui::text::{TextSpacing, UNITS_PER_COLUMN};
use nanobug::plugins::session::{ConfigScope, ConfigView};
use std::{
    cell::{Ref, RefCell},
    collections::BTreeMap,
    ops::Range,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::grid) enum Source {
    Local,
    External,
    Preserved,
}

impl Source {
    /// Shared message key for the action row and its preview heading.
    pub(in crate::grid) fn title(self) -> &'static str {
        match self {
            Self::Local => "plugin_manager.compare_local",
            Self::External => "plugin_manager.compare_external",
            Self::Preserved => "plugin_manager.compare_preserved",
        }
    }
}

/// Pin the compared versions so the resolution action uses the revision the user reviewed.
pub(in crate::grid) struct Comparison {
    pub scope: ConfigScope,
    pub configurations: Arc<BTreeMap<ConfigScope, ConfigView>>,
    pub from: (Detail, usize, usize),
    pub preview: Option<Preview>,
}

impl Comparison {
    /// The snapshot is shared with the published session view and always contains this scope.
    pub(in crate::grid) fn config(&self) -> &ConfigView {
        &self.configurations[&self.scope]
    }

    pub(in crate::grid) fn open(
        &mut self,
        source: Source,
        selection: usize,
        offset: usize,
    ) -> bool {
        let config = self.config();
        let text = match source {
            Source::Local => {
                serde_json::to_string_pretty(config.draft.as_ref().unwrap_or(&config.applied)).ok()
            }
            Source::External => config
                .external
                .as_ref()
                .and_then(|config| serde_json::to_string_pretty(config).ok()),
            Source::Preserved => config.preserved_external.as_ref().map(|path| {
                path.display()
                    .to_string()
                    .replace('\r', "\\r")
                    .replace('\n', "\\n")
            }),
        };
        let Some(text) = text else {
            return false;
        };
        self.preview = Some(Preview {
            source,
            from: (selection, offset),
            text,
            lines: RefCell::default(),
        });
        true
    }
}

pub(in crate::grid) struct Preview {
    pub source: Source,
    pub from: (usize, usize),
    text: String,
    lines: RefCell<Lines>,
}

#[derive(Default)]
struct Lines {
    width: usize,
    ranges: Vec<Range<usize>>,
}

impl Preview {
    /// Reflow when the pane width changes. All ranges remain slices of the original text.
    pub(in crate::grid) fn lines(
        &self,
        columns: usize,
        spacing: &TextSpacing,
    ) -> Ref<'_, [Range<usize>]> {
        let width = columns.max(1) * UNITS_PER_COLUMN;
        if self.lines.borrow().width != width {
            let mut lines = self.lines.borrow_mut();
            lines.width = width;
            lines.ranges.clear();
            let mut base = 0;
            for line in self.text.split('\n') {
                let visible = line.strip_suffix('\r').unwrap_or(line);
                if visible.is_empty() {
                    lines.ranges.push(base..base);
                }
                let mut start = 0;
                while start < visible.len() {
                    let rest = &visible[start..];
                    let length = spacing
                        .cells(rest)
                        .take_while(|cell| cell.end <= width)
                        .last()
                        .map_or_else(
                            || rest.graphemes(true).next().unwrap_or_default().len(),
                            |cell| cell.byte + cell.text.len(),
                        );
                    lines.ranges.push(base + start..base + start + length);
                    start += length;
                }
                base += line.len() + 1;
            }
        }
        Ref::map(self.lines.borrow(), |lines| lines.ranges.as_slice())
    }

    pub(in crate::grid) fn line(&self, range: Range<usize>) -> &str {
        &self.text[range]
    }
}
