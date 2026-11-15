//! Route scrolling and link activation to the visible Markdown preview.

use gpui::{Context, Pixels, Point, Window, px};
use nanobug::{animation::scroll::ScrollInput, i18n};
use std::path::{Path, PathBuf};
use url::{ParseError, Url};

use super::{Workspace, editor_splits::PaneId};
use crate::grid::{
    render::Rect,
    windows::editor::{MarkdownPreview, MarkdownViewMode},
};

/// An activated Markdown link after resolving it against the source document.
#[derive(Debug, PartialEq, Eq)]
enum LinkTarget {
    /// Web or email address handed to the platform URL handler.
    External(Url),
    /// URL fragment within the current document.
    Anchor(String),
    /// File opened by the editor, optionally followed by a preview anchor.
    File(PathBuf, Option<String>),
}

/// Resolve `href` using `source` for file links.
fn link_target(href: &str, source: Option<&Path>) -> Option<LinkTarget> {
    if href.is_empty()
        || href.starts_with('/')
        || href.contains('\\')
        || href.chars().any(char::is_control)
    {
        return None;
    }
    if let Some(fragment) = href.strip_prefix('#') {
        return Some(LinkTarget::Anchor(fragment.into()));
    }
    match Url::parse(href) {
        Ok(url) if matches!(url.scheme(), "http" | "https" | "mailto") => {
            Some(LinkTarget::External(url))
        }
        Err(ParseError::RelativeUrlWithoutBase) => {
            let source = Url::from_file_path(source?).ok()?;
            let target = source.join(href).ok()?;
            let path = target.to_file_path().ok()?;
            Some(LinkTarget::File(path, target.fragment().map(str::to_owned)))
        }
        _ => None,
    }
}

/// Validate an absolute web or mail link from transient Markdown without a source document.
pub(super) fn external_markdown_url(href: &str) -> Option<Url> {
    match link_target(href, None)? {
        LinkTarget::External(url) => Some(url),
        _ => None,
    }
}

impl Workspace {
    /// Prepare the preview at the editor's current size, using its own UI-thread cache.
    fn prepare_editor_markdown_preview(
        &mut self,
        pane: PaneId,
        area: Rect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<MarkdownPreview> {
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let project = self.projects.active_project_mut()?;
        let cache = if pane == project.active_editor_id() {
            &mut self.markdown_cache
        } else {
            &mut self
                .inactive_editor_panels
                .iter_mut()
                .find(|entry| entry.pane == pane)?
                .markdown
        };
        project
            .editor_by_id_mut(pane)?
            .markdown_preview(cache, area, self.metrics, now, window, cx)
    }

    /// Prepare the active file's preview for navigation or an explicit scroll operation.
    pub(super) fn prepare_markdown_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<MarkdownPreview> {
        let pane = self.projects.active_project()?.active_editor_id();
        let preview =
            self.prepare_editor_markdown_preview(pane, self.active_editor_area(), window, cx)?;
        self.schedule_scroll(window, cx);
        Some(preview)
    }

    /// Prepare previews for all visible editing panes and retain their IDs for link hit testing.
    pub(super) fn prepare_markdown_previews(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(PaneId, MarkdownPreview)> {
        let previews: Vec<_> = self
            .editor_rects()
            .into_iter()
            .filter_map(|(pane, area)| {
                self.prepare_editor_markdown_preview(pane, area, window, cx)
                    .map(|preview| (pane, preview))
            })
            .collect();
        if !previews.is_empty() {
            self.schedule_scroll(window, cx);
        }
        previews
    }

    /// Number of grid rows covered by one preview page at the current window size.
    pub(super) fn markdown_preview_page_rows(&self) -> usize {
        self.projects
            .active()
            .editor
            .markdown_preview_area(self.active_editor_area())
            .map_or(1, |area| area.h.saturating_sub(1).max(1))
    }

    /// Apply a preview key in grid rows. Explicit positions are logical pixels.
    pub(super) fn scroll_markdown_preview(
        &mut self,
        input: ScrollInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match input {
            ScrollInput::Wheel(rows) => {
                ScrollInput::Wheel((rows as f32 * self.metrics.lh).round() as isize)
            }
            input => input,
        };
        let pane = self.projects.active().active_editor_id();
        self.update_markdown_scroll(pane, self.active_editor_area(), input, window, cx);
    }

    /// Scroll a preview under the pointer without moving the editor's source caret.
    pub(super) fn wheel_markdown_preview(
        &mut self,
        position: Point<Pixels>,
        vertical: Pixels,
        horizontal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.modal_open() {
            return false;
        }
        let Some(project) = self.projects.active_project() else {
            return false;
        };
        let Some((pane, area)) = self.editor_rects().into_iter().find(|&(pane, area)| {
            project
                .editor_by_id(pane)
                .and_then(|editor| editor.markdown_preview_area(area))
                .is_some_and(|preview| preview.pixels(self.metrics).contains(&position))
        }) else {
            return false;
        };
        let editor = self
            .projects
            .active_mut()
            .editor_by_id_mut(pane)
            .expect("visible editor");
        if !horizontal && vertical != px(0.) && editor.composition().is_empty() {
            let delta = editor.markdown_wheel_pixels(-f32::from(vertical));
            self.update_markdown_scroll(pane, area, ScrollInput::Wheel(delta), window, cx);
        }
        true
    }

    /// Activate a clicked `href` in the current preview through `window` and `cx`.
    pub(super) fn open_markdown_link(
        &mut self,
        href: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self
            .projects
            .active_project()
            .and_then(|project| project.editor.vim().doc.path.as_deref());
        match link_target(href, source) {
            Some(LinkTarget::External(url)) => cx.open_url(url.as_str()),
            Some(LinkTarget::Anchor(fragment)) => {
                self.reveal_markdown_anchor(&fragment, window, cx)
            }
            Some(LinkTarget::File(path, fragment)) => {
                let Some(path) = path.canonicalize().ok().filter(|path| path.is_file()) else {
                    self.set_status(i18n::format(
                        "vim_g.missing_file",
                        &[("path", &path.display())],
                    ));
                    self.refresh_footer(cx);
                    return;
                };
                if source != Some(path.as_path()) {
                    self.finish_editor_input(window, cx);
                    self.open(path.clone(), window, cx);
                }
                let editor = &mut self.projects.active_mut().editor;
                let fragment = fragment.filter(|_| {
                    editor.vim().doc.path.as_deref() == Some(path.as_path())
                        && editor.markdown_view_mode().is_some()
                });
                if fragment.is_some() && editor.markdown_view_mode() == Some(MarkdownViewMode::Edit)
                {
                    editor.set_markdown_view_mode(MarkdownViewMode::Split);
                }
                self.sync(window, cx);
                if let Some(fragment) = fragment {
                    self.reveal_markdown_anchor(&fragment, window, cx);
                }
            }
            None => {}
        }
    }

    /// Smoothly reveal `fragment` in the active document's preview.
    fn reveal_markdown_anchor(
        &mut self,
        fragment: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(top) = self
            .prepare_markdown_preview(window, cx)
            .and_then(|preview| preview.anchor_top(fragment))
        {
            self.scroll_markdown_preview(ScrollInput::Position(top), window, cx);
        }
    }

    /// Apply scroll input to `pane` at `area` and join the shared animation frame loop.
    fn update_markdown_scroll(
        &mut self,
        pane: PaneId,
        area: Rect,
        input: ScrollInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .prepare_editor_markdown_preview(pane, area, window, cx)
            .is_none()
        {
            return;
        }
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        self.projects
            .active_mut()
            .editor_by_id_mut(pane)
            .expect("prepared editor")
            .track_markdown_preview(input, now);
        self.schedule_scroll(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve encoded paths and fragments against the containing document on each platform.
    #[test]
    fn markdown_links_resolve_web_addresses_and_document_relative_paths() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let original = temporary.path().join("docs").join("README.md");
        assert_eq!(
            link_target("../Guide%20中文版.md?raw=true#part%20one", Some(&original)),
            Some(LinkTarget::File(
                temporary.path().join("Guide 中文版.md"),
                Some("part%20one".into()),
            ))
        );
        assert_eq!(
            link_target("#chapter", Some(&original)),
            Some(LinkTarget::Anchor("chapter".into()))
        );
        for address in ["https://example.org/a#b", "mailto:me@example.org"] {
            assert_eq!(
                link_target(address, None),
                Some(LinkTarget::External(Url::parse(address)?)),
            );
        }
        for unsupported in [
            "//remote.example/payload",
            "file:///C:/payload.exe",
            "javascript:alert(1)",
            "/payload.exe",
            "D:\\payload.exe",
        ] {
            assert_eq!(link_target(unsupported, Some(&original)), None);
        }
        Ok(())
    }
}
