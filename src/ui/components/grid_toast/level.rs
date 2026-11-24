use crate::ui::{
    icons::Icon,
    text::TextSpacing,
    theme::{ERROR_BORDER, ERROR_TEXT, MUTED, TEXT, WARNING_BORDER, WARNING_TEXT},
};
use nanobug::i18n;

/// Space for a severity icon and the gap before its title.
pub(crate) const MESSAGE_ICON_COLUMNS: usize = 3;
/// Filled warning triangle with a transparent exclamation mark.
const WARNING_ICON: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/message-warning.svg"),
    1.,
);
/// Filled error circle with a transparent cross.
const ERROR_ICON: Icon = Icon::new(
    include_bytes!("../../../../assets/icons/message-error.svg"),
    1.,
);

/// Severity of feedback drawn in a message frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageLevel {
    /// Progress or a successful result.
    Info,
    /// An action needs a different selection or input.
    Warning,
    /// An action failed.
    Error,
}

impl MessageLevel {
    /// Return the RGB outline and text colors in that order.
    pub fn colors(self) -> (u32, u32) {
        match self {
            Self::Info => (MUTED, TEXT),
            Self::Warning => (WARNING_BORDER, WARNING_TEXT),
            Self::Error => (ERROR_BORDER, ERROR_TEXT),
        }
    }

    /// Return the icon and localized title displayed in the outline.
    pub(crate) fn heading(self) -> Option<(Icon, &'static str)> {
        match self {
            Self::Info => None,
            Self::Warning => Some((WARNING_ICON, i18n::text("message.warning"))),
            Self::Error => Some((ERROR_ICON, i18n::text("message.error"))),
        }
    }

    /// Measure the severity icon and localized title in the grid's text `spacing`.
    pub fn heading_columns(self, spacing: &TextSpacing) -> usize {
        self.heading().map_or(0, |(_, title)| {
            MESSAGE_ICON_COLUMNS + spacing.columns(title)
        })
    }
}
