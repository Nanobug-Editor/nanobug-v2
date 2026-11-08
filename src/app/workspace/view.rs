//! Compose retained panels, scrolling clips, the cursor and the shortcut-help backdrop.

use super::{
    Editor,
    buffer::TEXT_OVERSCAN,
    layout::{FOOTER_ROWS, Focus, HELP_CORNER_RADIUS, ShellWindow, help_outline},
};
use crate::ui::{
    backdrop::BackdropRaster,
    grid::{Panel, Rect},
    theme::BACKGROUND,
};
use gpui::{
    App, Bounds, Context, Corners, ElementInputHandler, Entity, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Render, RenderImage, Size, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, canvas, div, point, px, rgba, size,
};
use std::sync::Arc;

/// Placement of a retained workspace panel and its optional clip.
struct GridLayer {
    /// Panel that owns the shaped content.
    panel: Entity<Panel>,
    /// Window-local bounds of the panel's content, including its scroll offset.
    bounds: Bounds<Pixels>,
    /// Fixed viewport for a scrollable body.
    clip: Option<Bounds<Pixels>>,
}

/// One cached, window-local backdrop while shortcut help is open.
pub(super) struct HelpBackdrop {
    /// Logical size of the window the image covers.
    viewport: Size<Pixels>,
    /// Rounded popup bounds reserved in the image.
    outline: Bounds<Pixels>,
    /// Downsampled, blurred workspace texture.
    image: Arc<RenderImage>,
}

impl Editor {
    /// Place fixed panels and scrollable bodies, keeping the footer aligned to the bottom inset.
    fn grid_layers(&self) -> [GridLayer; 7] {
        let fixed = |panel: &Entity<Panel>, rect: Rect| GridLayer {
            panel: panel.clone(),
            bounds: rect.pixels(self.metrics),
            clip: None,
        };
        let scrolled = |panel: &Entity<Panel>, rect: Rect, fraction: f32| {
            let clip = rect.pixels(self.metrics);
            GridLayer {
                panel: panel.clone(),
                bounds: Bounds::new(
                    clip.origin - point(px(0.), px(fraction * self.metrics.lh)),
                    size(clip.size.width, px((rect.h + 1) as f32 * self.metrics.lh)),
                ),
                clip: Some(clip),
            }
        };
        let buffer = &self.buffers[self.active];
        let body = self.editor_text();
        let gutter = self.layout.editor_body();
        let gutter = Rect {
            w: body.x - gutter.x,
            ..gutter
        };
        let mut text = scrolled(&self.bodies[2], body, buffer.scroll.fraction());
        if body.w > 0 {
            text.bounds.origin.x -= px(buffer.text_displacement() * self.metrics.cw);
            text.bounds.size.width +=
                px((buffer.left_padding() + TEXT_OVERSCAN) as f32 * self.metrics.cw);
        }
        let mut footer = fixed(
            &self.panels[3],
            Rect {
                x: 0,
                y: self.layout.rows - FOOTER_ROWS,
                w: self.layout.cols,
                h: FOOTER_ROWS,
            },
        );
        footer.bounds.origin.y += self.layout.footer_offset;
        [
            fixed(
                &self.panels[0],
                Rect {
                    x: 0,
                    y: 0,
                    w: self.layout.cols,
                    h: 1,
                },
            ),
            fixed(&self.panels[1], self.layout.tree),
            fixed(&self.panels[2], self.layout.editor),
            footer,
            scrolled(
                &self.bodies[0],
                self.layout.tree_body(),
                self.tree_scroll.fraction(),
            ),
            scrolled(&self.bodies[1], gutter, buffer.scroll.fraction()),
            text,
        ]
    }

    /// Position the preview over its scrolled tree row, clipping vertically to the tree body.
    fn tree_name_preview_layer(&self) -> Option<GridLayer> {
        let mut bounds = self.tree_name_preview_rect?.pixels(self.metrics);
        bounds.origin.y -= px(self.tree_scroll.fraction() * self.metrics.lh);
        let body = self.layout.tree_body().pixels(self.metrics);
        let clip = bounds.intersect(&Bounds::new(
            point(bounds.origin.x, body.origin.y),
            size(bounds.size.width, body.size.height),
        ));
        (!clip.is_empty()).then(|| GridLayer {
            panel: self.tree_name_preview.clone(),
            bounds,
            clip: Some(clip),
        })
    }

    /// Reuse the blurred `layers` while the `outline` and `window` size match.
    /// `cx` supplies the retained panels and their SVG renderer.
    fn help_backdrop_image(
        &mut self,
        layers: &[GridLayer],
        outline: Bounds<Pixels>,
        window: &mut Window,
        cx: &App,
    ) -> Arc<RenderImage> {
        let viewport = window.viewport_size();
        if let Some(backdrop) = &self.help_backdrop
            && backdrop.viewport == viewport
            && backdrop.outline == outline
        {
            return backdrop.image.clone();
        }
        let mut raster = BackdropRaster::new(viewport, self.glyphs.as_ref(), cx.svg_renderer());
        for layer in layers {
            let clip = layer.clip.unwrap_or(layer.bounds);
            if !clip.is_empty() {
                layer
                    .panel
                    .read(cx)
                    .stamp_backdrop(layer.bounds, clip, &mut raster, window);
            }
        }
        let image = raster.finish(outline, HELP_CORNER_RADIUS);
        if let Some(previous) = self.help_backdrop.replace(HelpBackdrop {
            viewport,
            outline,
            image: image.clone(),
        }) {
            let _ = window.drop_image(previous.image);
        }
        image
    }
}

/// Position a cached panel using the logical `bounds` relative to its parent.
fn positioned(bounds: Bounds<Pixels>) -> StyleRefinement {
    StyleRefinement::default()
        .absolute()
        .left(bounds.origin.x)
        .top(bounds.origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
}

impl Render for Editor {
    /// Compose the workspace and optional frosted shortcut help.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.derive_layout(window) != self.layout {
            self.sync(window, cx);
        }
        if !window.is_window_hovered() {
            self.set_border_hover(None, cx);
        }
        let mut root = div()
            .id("nanobug")
            .relative()
            .size_full()
            .bg(rgba(BACKGROUND))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_exit(cx.listener(|editor, _, _, cx| editor.set_border_hover(None, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll));
        let layers = self.grid_layers();
        if self.help_top.is_some() {
            let grid = self.help_layout().0.pixels(self.metrics);
            let outline = help_outline(grid, self.metrics);
            let backdrop = self.help_backdrop_image(&layers, outline, window, cx);
            root = root
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let _ = window.paint_image(
                                bounds,
                                bounds,
                                Corners::default(),
                                backdrop.clone(),
                                0,
                                false,
                            );
                        },
                    )
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full(),
                )
                .child(self.help_panel.clone().cached(positioned(grid)));
        } else {
            if let Some(backdrop) = self.help_backdrop.take() {
                let _ = window.drop_image(backdrop.image);
            }
            for GridLayer {
                panel,
                bounds,
                clip,
            } in layers.into_iter().chain(self.tree_name_preview_layer())
            {
                if bounds.is_empty() {
                    continue;
                }
                root = if let Some(clip) = clip {
                    let local = Bounds::new(bounds.origin - clip.origin, bounds.size);
                    root.child(
                        div()
                            .absolute()
                            .left(clip.origin.x)
                            .top(clip.origin.y)
                            .w(clip.size.width)
                            .h(clip.size.height)
                            .overflow_hidden()
                            .child(panel.cached(positioned(local))),
                    )
                } else {
                    root.child(panel.cached(positioned(bounds)))
                };
            }
            root = root.child(
                self.cursor.clone().cached(
                    StyleRefinement::default()
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full(),
                ),
            );
        }
        if let Some(target) = self.hovered_border
            && let Some(button) = self.window_area(target).close_button()
        {
            let bounds = button.pixels(self.metrics);
            let id = match target {
                ShellWindow::Pane(Focus::Manager) => "close-files",
                ShellWindow::Pane(Focus::Editor) => "close-editor",
                ShellWindow::ShortcutHelp => "close-shortcuts",
            };
            root = root.child(
                div()
                    .id(id)
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |editor, event, window, cx| {
                        editor.click_close_button(target, event, window, cx);
                    }))
                    .child(self.close_glyph.element()),
            );
        }
        let view = cx.entity();
        root.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    view.update(cx, |editor, cx| {
                        window.handle_input(
                            &editor.focus_handle,
                            ElementInputHandler::new(bounds, cx.entity()),
                            cx,
                        )
                    });
                },
            )
            .absolute()
            .left_0()
            .top_0()
            .size_full(),
        )
    }
}
