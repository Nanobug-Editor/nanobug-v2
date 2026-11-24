use crate::ui::text::TextSpacing;

/// A footer's ordered captions and the number of columns they occupy.
pub(crate) struct GridKeyHints {
    pub(crate) labels: Vec<String>,
    pub(crate) width: usize,
    limit: usize,
}

impl GridKeyHints {
    pub(crate) const SEPARATOR: &'static str = " | ";

    /// Start an empty row within the available column budget.
    pub(crate) fn new(columns: usize) -> Self {
        Self {
            labels: Vec::new(),
            width: 0,
            limit: columns,
        }
    }

    /// Append a complete localized caption and its optional separator when both fit.
    pub(crate) fn push(&mut self, action: &str, key: &str, spacing: &TextSpacing) -> bool {
        let label = format!("{action}: {key}");
        let gap = if self.labels.is_empty() {
            0
        } else {
            spacing.columns(Self::SEPARATOR)
        };
        let next = self.width + gap + spacing.columns(&label);
        if next > self.limit {
            return false;
        }
        self.width = next;
        self.labels.push(label);
        true
    }

    /// Fit a prefix of the caller's action list, preserving its display priority.
    pub(crate) fn fit<'a>(
        columns: usize,
        items: impl IntoIterator<Item = (&'a str, &'a str)>,
        spacing: &TextSpacing,
    ) -> Self {
        let mut row = Self::new(columns);
        for (action, key) in items {
            if !row.push(action, key, spacing) {
                break;
            }
        }
        row
    }
}
