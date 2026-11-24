use super::MessageLevel;
use crate::ui::{
    grid_geometry::Rect,
    text::{TextSpacing, UNITS_PER_COLUMN},
};
use gpui::{Context, Task, Window};
use std::{borrow::Cow, time::Duration};

/// Passive feedback. Dropping or replacing it cancels its pending dismissal.
pub(crate) struct GridToast {
    /// Severity used by the renderer for the border and title.
    pub(crate) level: MessageLevel,
    /// Single logical line of localized feedback.
    text: String,
    /// Retains the timer only while this message is displayed.
    _dismiss: Task<()>,
}

impl GridToast {
    /// Show `text` at `level` in `window` through `cx`. Blank text clears the toast.
    /// Call `dismiss` on the owning view after two seconds if this toast is still alive.
    pub(crate) fn show<V: 'static>(
        level: MessageLevel,
        text: String,
        window: &Window,
        cx: &mut Context<V>,
        dismiss: impl FnOnce(&mut V, &mut Context<V>) + 'static,
    ) -> Option<Self> {
        let text = text.replace(['\r', '\n', '\t'], " ");
        if text.trim().is_empty() {
            return None;
        }
        let delay = cx.background_executor().timer(Duration::from_secs(2));
        let task = cx.spawn_in(window, async move |view, cx| {
            delay.await;
            let _ = cx.update(|_, cx| {
                let _ = view.update(cx, dismiss);
            });
        });
        Some(Self {
            level,
            text,
            _dismiss: task,
        })
    }

    /// Wrap text into at most three rows at the upper-right of `viewport`.
    /// `available` limits the full width and `spacing` measures graphemes and the severity heading.
    pub(crate) fn layout(
        &self,
        viewport: Rect,
        available: usize,
        spacing: &TextSpacing,
    ) -> (Rect, Vec<Cow<'_, str>>) {
        let width = spacing
            .columns(&self.text)
            .max(self.level.heading_columns(spacing))
            .min(available.min(viewport.w).saturating_sub(4))
            .min(54);
        let mut remaining = self.text.as_str();
        let mut lines = Vec::new();
        for _ in 0..2 {
            let end = spacing
                .fitting_cells(remaining, width * UNITS_PER_COLUMN)
                .last()
                .map_or(0, |cell| cell.byte + cell.text.len());
            if end == 0 || end == remaining.len() {
                break;
            }
            let end = if remaining[end..].starts_with(' ') {
                end
            } else {
                remaining[..end]
                    .rfind(' ')
                    .filter(|&split| split > 0)
                    .unwrap_or(end)
            };
            lines.push(Cow::Borrowed(&remaining[..end]));
            remaining = remaining[end..].trim_start();
        }
        lines.push(spacing.ellipsize(remaining, width));
        let w = (width + 4).min(available).min(viewport.w);
        let h = (lines.len() + 2).min(viewport.h);
        let area = Rect {
            x: viewport.x + viewport.w.saturating_sub(w + 1),
            y: viewport.y + 1.min(viewport.h - h),
            w,
            h,
        };
        (area, lines)
    }
}
