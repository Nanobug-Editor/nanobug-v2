//! Page-local Markdown presentation and the active document's native preview cache.

mod highlight;
mod parser;
mod render;
mod sync;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui::{AnyElement, App, Bounds, Pixels, Point, Window, point, px};
use nanobug::animation::scroll::{GridScroll, ScrollInput};
use pulldown_cmark::Options;

use super::EditorWindow;
use crate::{
    grid::render::{Metrics, Rect},
    ui::theme::{Style, style},
};
use sync::ScrollMap;
pub(super) use {highlight::MarkdownHighlights, render::PreviewLayout};

/// Common extensions enabled for both preview parsing and source coloring.
pub(super) const MARKDOWN_OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_GFM);

/// Return whether `path` has one of the supported Markdown extensions.
pub(super) fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["md", "markdown", "mdown", "mkd", "mkdn", "mdwn"]
                .into_iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

/// Arrangement of the source and preview inside a Markdown file's editor pane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub(in crate::grid) enum MarkdownViewMode {
    /// Show the normal source editor.
    Edit,
    /// Use the whole editor body for the rendered document.
    Preview,
    /// Keep the editable source on the left and the rendered document on the right.
    #[default]
    Split,
}

/// Displayed offsets where the current preview-led animation began.
#[derive(Clone, Copy)]
pub(super) struct PreviewScrollOrigin {
    /// Preview position in logical pixels.
    preview: f32,
    /// Editor position in visible rows.
    source: f32,
}

/// The active page's Markdown view as materialized on its selected buffer.
#[derive(Default)]
pub(super) struct MarkdownViewport {
    /// Mode restored when returning to this file on the page.
    pub(super) mode: MarkdownViewMode,
    /// Preview offset in logical pixels, animated by its own input or the source viewport.
    pub(super) scroll: GridScroll,
    /// Last scroll position at which the bottom of the preview is visible.
    maximum: usize,
    /// Fractional logical pixels carried between continuous wheel events.
    pub(super) wheel: f32,
    /// Source caret at the moment the preview took control of vertical scrolling.
    pub(super) preview_caret: Option<usize>,
    /// Paired starting offsets for the preview's current destination.
    pub(super) preview_scroll_origin: Option<PreviewScrollOrigin>,
    /// Reflow changed the correspondence of the preview's current offset to the source.
    pub(super) source_sync_pending: bool,
}

/// Give the source a usable gutter even in the narrowest split layout.
const MIN_SOURCE_COLUMNS: usize = 8;

/// Identifies shaped preview content, including metrics and foreground styles.
#[derive(PartialEq)]
struct LayoutKey {
    /// Opening identity and revised source text.
    document: (u64, u64),
    /// Source location used to resolve relative images.
    path: Option<PathBuf>,
    /// Width available for reflow.
    width: Pixels,
    /// Split preview uses the divider column and one Chinese glyph of horizontal inset.
    split: bool,
    /// Preview font size in logical pixels.
    font_size: f32,
    /// Native display scaling for the text shaper.
    scale: f32,
    /// Palette used for the shaped runs.
    style: Style,
    /// Resolved editor foreground also detects live palette file updates.
    foreground: u32,
    /// Last settled remote image state represented by this layout.
    image_revision: u64,
}

/// Retain parsed blocks across zoom, appearance and window-width changes.
#[derive(Default)]
pub(in crate::grid) struct MarkdownCache {
    /// Parsed source and the opening/revision that produced it.
    parsed: Option<((u64, u64), parser::Markdown)>,
    /// Width-dependent text and background geometry.
    layout: Option<(LayoutKey, Rc<PreviewLayout>)>,
}

/// Parsed Markdown for an embedded documentation panel, with its latest native layout.
pub(in crate::grid) struct MarkdownSnippet {
    /// Original provider text available for panel sizing.
    text: String,
    /// Parsed document blocks reused when the panel resizes.
    parsed: parser::Markdown,
    /// Most recent layout and the metrics that produced it.
    layout: RefCell<Option<(SnippetLayoutKey, Rc<PreviewLayout>)>>,
}

/// Geometry and palette that affect a documentation panel's layout.
#[derive(PartialEq)]
struct SnippetLayoutKey {
    /// Available body width before wrapping.
    width: Pixels,
    /// Configured editor font size in logical pixels.
    font_size: f32,
    /// Native display scaling used by the text system.
    scale: f32,
    /// Effective editor font and palette.
    style: Style,
    /// Resolved foreground also detects changes from live theme files.
    foreground: u32,
}

impl MarkdownSnippet {
    /// Parse the provider's Markdown `text` once and defer shaping until the panel is fitted.
    pub(in crate::grid) fn new(text: String) -> Self {
        Self {
            parsed: parser::parse(&text),
            text,
            layout: RefCell::new(None),
        }
    }

    /// Inspect the original Markdown passed to this panel.
    pub(in crate::grid) fn text(&self) -> &str {
        &self.text
    }

    /// Shape at the available body `width` and editor `font_size` for `window` and `cx`.
    pub(super) fn layout(
        &self,
        width: Pixels,
        font_size: f32,
        window: &Window,
        cx: &App,
    ) -> Rc<PreviewLayout> {
        let theme = style(cx).for_component(crate::ui::theme::ThemeComponent::Editor);
        let key = SnippetLayoutKey {
            width,
            font_size,
            scale: window.scale_factor(),
            style: theme,
            foreground: theme.foreground(crate::ui::theme::TEXT),
        };
        let mut cached = self.layout.borrow_mut();
        if let Some((_, layout)) = cached.as_ref().filter(|(old, _)| *old == key) {
            return layout.clone();
        }
        let layout = Rc::new(PreviewLayout::snippet(
            &self.parsed,
            width,
            font_size,
            theme,
            window,
        ));
        *cached = Some((key, layout.clone()));
        layout
    }
}

/// Visible preview positioned in the workspace and ready to paint through an overlay clip.
pub(in crate::grid) struct MarkdownPreview {
    /// Full viewport before intersecting it with any popup cutouts.
    bounds: Bounds<Pixels>,
    /// Current animated offset in logical pixels.
    offset: Pixels,
    /// Shaped content shared by all clipped pieces of the viewport.
    layout: Rc<PreviewLayout>,
}

impl MarkdownPreview {
    /// Paint the preview inside the optional workspace overlay clip.
    pub(in crate::grid) fn element(&self, clip: Option<Bounds<Pixels>>) -> Option<AnyElement> {
        let clip = clip.map_or(self.bounds, |clip| self.bounds.intersect(&clip));
        (!clip.is_empty()).then(|| self.layout.element(self.bounds, clip, self.offset))
    }

    /// Return a parsed link destination under the workspace pointer `position`.
    pub(in crate::grid) fn link_at(&self, position: Point<Pixels>) -> Option<&str> {
        if !self.bounds.contains(&position) {
            return None;
        }
        self.layout
            .link_at(position - self.bounds.origin + point(px(0.), self.offset))
    }

    /// Resolve a URL `fragment` to the preview scroll offset in logical pixels.
    pub(in crate::grid) fn anchor_top(&self, fragment: &str) -> Option<usize> {
        self.layout.anchor_top(fragment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanobug::document::Document;

    /// Keep source geometry and Markdown mode independent across Tabpages sharing a buffer.
    #[test]
    fn markdown_layout_belongs_to_the_page_view() {
        let area = Rect {
            x: 4,
            y: 3,
            w: 50,
            h: 20,
        };
        let mut doc = Document::new("# Title");
        doc.path = Some("README.MD".into());
        let read_only = EditorWindow::read_only(doc.saved_version());
        let full_source = read_only.text_area(area);
        assert!(read_only.markdown_preview_area(area).is_none());
        let mut editor = EditorWindow::new(doc);
        assert_eq!(editor.markdown_view_mode(), Some(MarkdownViewMode::Split));
        let source = editor.text_area(area);
        let preview = editor.markdown_preview_area(area).unwrap();
        assert_eq!((source.y, source.h), (preview.y, preview.h));
        assert_eq!(source.x + source.w + 2, preview.x);
        assert_eq!(preview.x + preview.w, area.x + area.w - 1);
        let narrow = Rect { w: 12, ..area };
        assert_eq!(editor.text_area(narrow).w, 3);
        assert!(editor.set_markdown_view_mode(MarkdownViewMode::Edit));
        let other = EditorWindow::new(editor.vim().doc.fork());
        assert_eq!(other.markdown_view_mode(), Some(MarkdownViewMode::Split));
        assert_eq!(editor.markdown_view_mode(), Some(MarkdownViewMode::Edit));
        assert!(editor.text_area(area) == full_source);
        assert!(editor.set_markdown_view_mode(MarkdownViewMode::Preview));
        assert_eq!(editor.text_area(area).h, 0);
        assert!(editor.markdown_preview_area(area) == Some(EditorWindow::body(area)));
        assert_eq!(other.markdown_view_mode(), Some(MarkdownViewMode::Split));
        assert_eq!(other.vim().doc.text, "# Title");
    }
}

impl EditorWindow {
    /// Identify Markdown from the active file extension and return its window-local mode.
    pub(in crate::grid) fn markdown_view_mode(&self) -> Option<MarkdownViewMode> {
        if self.vim().is_read_only() {
            return None;
        }
        is_markdown(self.vim().doc.path.as_deref()?).then_some(self.buffer().markdown.mode)
    }

    /// Switch a Markdown file's arrangement, ending transient editor input.
    pub(in crate::grid) fn set_markdown_view_mode(&mut self, mode: MarkdownViewMode) -> bool {
        let Some(previous) = self.markdown_view_mode().filter(|&current| current != mode) else {
            return false;
        };
        self.end_window_selection();
        self.cancel_pending();
        self.composition.clear();
        self.stop_drag();
        let buffer = &mut self.buffers[self.active];
        buffer.markdown.mode = mode;
        buffer.markdown.preview_caret = (previous == MarkdownViewMode::Preview
            && mode == MarkdownViewMode::Split)
            .then_some(buffer.vim.doc.cursor);
        buffer.markdown.source_sync_pending = false;
        buffer.markdown.preview_scroll_origin = None;
        true
    }

    /// The editable body inside `area`, shortened when the preview has its own column.
    pub(super) fn source_body(&self, area: Rect) -> Rect {
        let body = Self::body(area);
        let w = match self.markdown_view_mode() {
            Some(MarkdownViewMode::Preview) => 0,
            Some(MarkdownViewMode::Split) if body.w >= 3 => {
                (body.w / 2).max(MIN_SOURCE_COLUMNS).min(body.w - 2)
            }
            _ => body.w,
        };
        Rect {
            w,
            h: if w == 0 { 0 } else { body.h },
            ..body
        }
    }

    /// The rendered column inside `area`, with a one-cell divider in split mode.
    pub(in crate::grid) fn markdown_preview_area(&self, area: Rect) -> Option<Rect> {
        let body = Self::body(area);
        let preview = match self.markdown_view_mode()? {
            MarkdownViewMode::Edit => return None,
            MarkdownViewMode::Preview => body,
            MarkdownViewMode::Split => {
                let start = self.source_body(area).w + 1;
                Rect {
                    x: body.x + start,
                    w: body.w.saturating_sub(start),
                    ..body
                }
            }
        };
        (preview.w > 0 && preview.h > 0).then_some(preview)
    }

    /// Prepare the active file at the visible preview width and clamp its saved offset.
    pub(in crate::grid) fn markdown_preview(
        &mut self,
        cache: &mut MarkdownCache,
        area: Rect,
        metrics: Metrics,
        now: f64,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<MarkdownPreview> {
        let bounds = self.markdown_preview_area(area)?.pixels(metrics);
        let split = self.markdown_view_mode() == Some(MarkdownViewMode::Split);
        let preview_leads = self.source_follows_markdown_preview(area);
        let source = self.text_area(area);
        let buffer = &mut self.buffers[self.active];
        let doc = &buffer.vim.doc;
        let appearance = style(cx);
        let key = LayoutKey {
            document: (doc.opening, doc.revision),
            path: doc.path.clone(),
            width: bounds.size.width,
            split,
            font_size: metrics.font_size,
            scale: window.scale_factor(),
            style: appearance,
            foreground: appearance
                .for_component(crate::ui::theme::ThemeComponent::Editor)
                .foreground(crate::ui::theme::TEXT),
            image_revision: render::image_revision(cx),
        };
        let reflow = cache.layout.as_ref().is_none_or(|(old, _)| *old != key);
        if reflow {
            if cache
                .parsed
                .as_ref()
                .is_none_or(|(old, _)| *old != key.document)
            {
                cache.parsed = Some((key.document, parser::parse(&doc.text)));
            }
            let (_, blocks) = cache.parsed.as_ref().expect("parsed active Markdown");
            let layout = PreviewLayout::new(
                blocks,
                key.width,
                key.split,
                key.font_size,
                key.path.as_deref(),
                window,
                cx,
            );
            cache.layout = Some((key, Rc::new(layout)));
        }
        let (_, layout) = cache.layout.as_ref().expect("laid out active Markdown");
        let maximum = f32::from(layout.height() - bounds.size.height).ceil() as usize;
        let preview_top = (split && !preview_leads)
            .then(|| {
                ScrollMap::new(
                    layout.scroll_points(),
                    buffer.cached_visual_rows(source.w),
                    source.h,
                    maximum,
                )
                .map(|map| map.preview_top(buffer.scroll.position()))
            })
            .flatten();
        let view = &mut buffer.markdown;
        let resized = view.maximum != maximum;
        view.source_sync_pending |= preview_leads && (reflow || resized);
        view.maximum = maximum;
        if let Some(preview_top) = preview_top {
            view.scroll
                .follow(preview_top, maximum, buffer.scroll.is_active(), now);
        } else if resized || view.scroll.target() > maximum {
            view.scroll.request(view.scroll.target(), maximum, now);
        }
        Some(MarkdownPreview {
            bounds,
            offset: px(view.scroll.position()),
            layout: layout.clone(),
        })
    }

    /// Scroll the active preview using logical pixels after its layout has been prepared.
    pub(in crate::grid) fn track_markdown_preview(&mut self, input: ScrollInput, now: f64) {
        let buffer = &mut self.buffers[self.active];
        let view = &mut buffer.markdown;
        let target = match input {
            ScrollInput::Wheel(pixels) => view.scroll.target().saturating_add_signed(pixels),
            ScrollInput::Position(top) | ScrollInput::PositionWithTail(top) => top,
            ScrollInput::Advance | ScrollInput::Reveal => view.scroll.target(),
        };
        let target = target.min(view.maximum);
        if view.mode == MarkdownViewMode::Split && target != view.scroll.target() {
            view.preview_caret = Some(buffer.vim.doc.cursor);
            view.preview_scroll_origin = Some(PreviewScrollOrigin {
                preview: view.scroll.position(),
                source: buffer.scroll.position(),
            });
        }
        view.scroll.request(target, view.maximum, now);
    }

    /// Whether the visible split is following direct preview input with an unmoved source caret.
    pub(super) fn source_follows_markdown_preview(&self, area: Rect) -> bool {
        self.markdown_view_mode() == Some(MarkdownViewMode::Split)
            && self.markdown_preview_area(area).is_some()
            && self.buffer().markdown.preview_caret == Some(self.vim().doc.cursor)
    }

    /// Let source text input or navigation reclaim vertical scrolling.
    pub(in crate::grid) fn markdown_source_takes_scroll(&mut self) {
        let view = &mut self.buffers[self.active].markdown;
        view.preview_caret = None;
        view.preview_scroll_origin = None;
        view.source_sync_pending = false;
    }

    /// Advance the preview in the shared frame loop and report any source viewport movement.
    pub(in crate::grid) fn advance_markdown_preview(
        &mut self,
        cache: &MarkdownCache,
        area: Rect,
        now: f64,
    ) -> (bool, f32) {
        let follows = self.source_follows_markdown_preview(area);
        let preview_only = self.markdown_view_mode() == Some(MarkdownViewMode::Preview);
        if !follows && !preview_only || self.markdown_preview_area(area).is_none() {
            return (false, 0.);
        }
        let source = self.text_area(area);
        let buffer = &mut self.buffers[self.active];
        let view = &mut buffer.markdown;
        let preview_before = view.scroll.position();
        let moving = view.scroll.advance(now);
        if !follows {
            return (false, 0.);
        }
        let Some((key, layout)) = &cache.layout else {
            return (false, 0.);
        };
        let doc = &buffer.vim.doc;
        if key.document != (doc.opening, doc.revision) {
            return (false, 0.);
        }
        let reflow = std::mem::take(&mut view.source_sync_pending);
        let first = buffer.scroll.first_cell();
        let before = buffer.scroll.position();
        let origin =
            view.preview_scroll_origin
                .filter(|_| !reflow)
                .unwrap_or(PreviewScrollOrigin {
                    preview: preview_before,
                    source: before,
                });
        let (current, target, preview_maximum) = (
            view.scroll.position(),
            view.scroll.target() as f32,
            view.maximum,
        );
        let Some(map) = ScrollMap::new(
            layout.scroll_points(),
            buffer.cached_visual_rows(source.w),
            source.h,
            preview_maximum,
        ) else {
            return (false, 0.);
        };
        let (position, maximum) = map.follow_preview(origin, current, target);
        buffer.markdown.preview_scroll_origin = moving.then_some(origin);
        buffer.scroll.follow(position, maximum, moving, now);
        (
            first != buffer.scroll.first_cell(),
            buffer.scroll.position() - before,
        )
    }

    #[cfg(test)]
    /// Read the preview offset after the latest scroll frame in native regression tests.
    pub(in crate::grid) fn markdown_preview_scroll_position(&self) -> Option<f32> {
        match self.markdown_view_mode()? {
            MarkdownViewMode::Split | MarkdownViewMode::Preview => {
                Some(self.buffer().markdown.scroll.position())
            }
            MarkdownViewMode::Edit => None,
        }
    }

    /// Track wheel delta over the preview separately from the source editor.
    pub(in crate::grid) fn markdown_wheel_pixels(&mut self, delta: f32) -> isize {
        let wheel = &mut self.buffers[self.active].markdown.wheel;
        *wheel += delta;
        let pixels = wheel.trunc() as isize;
        *wheel -= pixels as f32;
        pixels
    }

    /// Whether the visible preview needs additional animation frames.
    pub(in crate::grid) fn markdown_scroll_active(&self, area: Rect) -> bool {
        let view = &self.buffer().markdown;
        self.markdown_preview_area(area).is_some()
            && (self.markdown_view_mode() == Some(MarkdownViewMode::Preview)
                || self.source_follows_markdown_preview(area))
            && (view.scroll.is_active() || view.source_sync_pending)
    }
}
