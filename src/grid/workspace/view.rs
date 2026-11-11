//! Compose retained panels, the cursor and workspace overlays.

use crate::grid::windows::{editor::EditorWindow, manager::ManagerWindow};

use crate::grid::render::{GridLayer, Metrics, Panel, Rect};
use crate::grid::windows::{
    editor::TEXT_OVERSCAN,
    help::{HelpWindow, help_outline},
    start::StartPage,
};

use super::{
    Workspace,
    language::language_list_bounds,
    layout::{FOOTER_ROWS, Focus, OUTER_INSET_Y, ShellWindow},
    leader_hint::exposed_regions,
};
use crate::ui::{
    icons::{Icon, IconCache},
    theme::BACKGROUND,
};
#[cfg(windows)]
use gpui::WindowControlArea;
use gpui::{
    AnyElement, Bounds, Context, Corners, ElementInputHandler, Entity, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Render, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, canvas, div, point, px, rgba, size,
};

/// Cross drawn in the border gap for a hovered pane or popup.
const CLOSE_ICON: Icon = Icon::new(include_bytes!("../../../assets/icons/pane-close.svg"), 1.);
/// The close icon's square size in logical pixels, excluding the clickable padding.
const CLOSE_ICON_SIZE: f32 = 12.;

impl Workspace {
    /// Place fixed panels and scrollable bodies, keeping the footer aligned to the bottom inset.
    fn grid_layers(&self) -> Option<[GridLayer; 6]> {
        let project = self.projects.active_project()?;
        let fixed = |panel: &Entity<Panel>, rect: Rect| GridLayer {
            panel: panel.clone(),
            bounds: rect.pixels(self.metrics),
            clip: None,
        };
        let scrolled =
            |panel, rect, fraction| GridLayer::scrolled(panel, rect, fraction, self.metrics);
        let buffer = &project.editor.buffers[project.editor.active];
        let body = project.editor.text_area(self.layout.editor);
        let gutter = EditorWindow::body(self.layout.editor);
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
            &self.panels[2],
            Rect {
                x: 0,
                y: self.layout.rows - FOOTER_ROWS,
                w: self.layout.cols,
                h: FOOTER_ROWS,
            },
        );
        footer.bounds.origin.y += self.layout.footer_offset;
        Some([
            fixed(&self.panels[0], self.layout.tree),
            fixed(&self.panels[1], self.layout.editor),
            footer,
            scrolled(
                &self.bodies[0],
                ManagerWindow::body(self.layout.tree),
                project
                    .manager
                    .as_ref()
                    .map_or(0., |manager| manager.scroll.fraction()),
            ),
            scrolled(&self.bodies[1], gutter, buffer.scroll.fraction()),
            text,
        ])
    }

    /// Position the preview over its scrolled tree row, clipping vertically to the tree body.
    fn tree_name_preview_layer(&self) -> Option<GridLayer> {
        let mut bounds = self.tree_name_preview_rect?.pixels(self.metrics);
        let manager = self.projects.active().manager.as_ref()?;
        bounds.origin.y -= px(manager.scroll.fraction() * self.metrics.lh);
        let body = ManagerWindow::body(self.layout.tree).pixels(self.metrics);
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
}

/// Position a cached panel using the logical `bounds` relative to its parent.
pub(super) fn positioned(bounds: Bounds<Pixels>) -> StyleRefinement {
    StyleRefinement::default()
        .absolute()
        .left(bounds.origin.x)
        .top(bounds.origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
}

/// Draw a retained `layer`, with an independent fixed clip for scrollable content.
fn layer_element(
    GridLayer {
        panel,
        bounds,
        clip,
    }: GridLayer,
) -> Option<AnyElement> {
    if bounds.is_empty() {
        return None;
    }
    Some(if let Some(clip) = clip {
        clipped_entity(panel, bounds, clip).into_any_element()
    } else {
        panel.cached(positioned(bounds)).into_any_element()
    })
}

/// Draw `entity` at its window-relative `bounds` through a fixed rectangular `clip`.
fn clipped_entity<V: Render>(
    entity: Entity<V>,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
) -> impl IntoElement {
    let local = Bounds::new(bounds.origin - clip.origin, bounds.size);
    div()
        .absolute()
        .left(clip.origin.x)
        .top(clip.origin.y)
        .w(clip.size.width)
        .h(clip.size.height)
        .overflow_hidden()
        .child(entity.cached(positioned(local)))
}

impl Render for Workspace {
    /// Compose the workspace with the active popup, if any.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.derive_layout(window) != self.layout {
            self.sync(window, cx);
        }
        if !window.is_window_hovered() {
            self.set_controls_hover(None, cx);
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
            .on_mouse_exit(
                cx.listener(|workspace, _, _, cx| workspace.set_controls_hover(None, cx)),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll));
        let layers = self
            .file_picker_layers()
            .or_else(|| self.grid_layers().map(Vec::from));
        let start = self
            .file_picker
            .is_none()
            .then(|| self.start_layout())
            .flatten()
            .map(|layout| GridLayer {
                panel: self.start_panel.clone(),
                bounds: layout
                    .area()
                    .pixels(StartPage::content_metrics(self.metrics)),
                clip: None,
            });
        let editor_close = self.editor_close_area();
        let language_picker = self.language_picker_area();
        let picker_filter = self.file_picker_filter_area();
        let small_popup = if self.help.is_open() {
            None
        } else {
            editor_close
                .or(language_picker)
                .or(picker_filter)
                .or_else(|| self.leader_hint_area())
        };
        if let Some(list) = &self.help.list {
            let area = self.help_layout();
            let grid = area.pixels(self.metrics);
            let outline = help_outline(grid, self.metrics);
            let filtering = list.is_filtering();
            let body = GridLayer::scrolled(
                &self.help.body,
                self.help.body(area),
                list.scroll.fraction(),
                HelpWindow::content_metrics(self.metrics),
            );
            let backdrop = self.help.backdrop_image(
                layers.as_deref().unwrap_or(&[]),
                outline,
                self.glyphs.as_ref(),
                window,
                cx,
            );
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
                .child(self.help.panel.clone().cached(positioned(grid)))
                .children(layer_element(body))
                .child(
                    div()
                        .absolute()
                        .left(grid.origin.x)
                        .top(grid.origin.y)
                        .w(grid.size.width)
                        .h(px(self.metrics.lh))
                        .opacity(0.5)
                        .child(
                            self.help
                                .dim_title
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        ),
                );
            if filtering {
                let search = HelpWindow::search_bounds(area, self.metrics);
                root = root.child(self.help.search.clone().cached(positioned(search)));
                let [apply, cancel] = HelpWindow::search_buttons(area, self.metrics, &self.spacing);
                for (id, bounds, cancel) in [
                    ("help-search-apply", apply, false),
                    ("help-search-cancel", cancel, true),
                ] {
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
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                workspace.finish_help_filter(cancel, window, cx);
                                cx.stop_propagation();
                            })),
                    );
                }
            }
        } else {
            self.help.release_backdrop(window);
            let preview = if self.file_picker.is_none() && layers.is_some() {
                self.tree_name_preview_layer()
            } else {
                None
            };
            if let Some(area) = small_popup {
                let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                let regions = exposed_regions(viewport, area.pixels(self.metrics));
                // Give each clip its own cached-view scope for the shared panels and cursor.
                for (index, region) in regions
                    .into_iter()
                    .enumerate()
                    .filter(|(_, region)| !region.is_empty())
                {
                    let visible = layers
                        .iter()
                        .flatten()
                        .chain(&start)
                        .chain(&preview)
                        .filter_map(|layer| {
                            let clip = region.intersect(&layer.clip.unwrap_or(layer.bounds));
                            if clip.is_empty() {
                                return None;
                            }
                            layer_element(GridLayer {
                                panel: layer.panel.clone(),
                                bounds: layer.bounds,
                                clip: Some(clip),
                            })
                        });
                    let mut underlay = div()
                        .id(("popup-underlay", index))
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full()
                        .children(visible);
                    if !self.small_modal_open() && picker_filter.is_none() {
                        underlay =
                            underlay.child(clipped_entity(self.cursor.clone(), viewport, region));
                    }
                    root = root.child(underlay);
                }
                if self.small_modal_open() || picker_filter.is_some() {
                    root = root.child(
                        self.popup_panel
                            .clone()
                            .cached(positioned(area.pixels(self.metrics))),
                    );
                    if language_picker.is_some() {
                        root = root.child(
                            self.language_list
                                .clone()
                                .cached(positioned(language_list_bounds(area, self.metrics))),
                        );
                    }
                } else {
                    root = root.child(self.leader_hint_element(area, cx));
                }
            } else {
                root = root.children(
                    layers
                        .into_iter()
                        .flatten()
                        .chain(start)
                        .chain(preview)
                        .filter_map(layer_element),
                );
            }
        }
        let tab_metrics = Metrics {
            origin: point(self.metrics.origin.x, px(OUTER_INSET_Y)),
            ..self.metrics
        };
        root = root.child(self.tabs.element(tab_metrics, window, cx));
        if editor_close.is_none()
            && (language_picker.is_some() || picker_filter.is_some() || small_popup.is_none())
        {
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
        #[cfg(windows)]
        {
            // The project strip doubles as the native caption hit area.
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(self.native_caption_height())
                    .window_control_area(WindowControlArea::Drag),
            );
        }
        if let Some(target) = self.hovered_controls
            && let Some(controls) = self.window_area(target).corner_controls(target.has_help())
        {
            let corner_button = |bounds: Bounds<Pixels>| {
                div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            };
            if let Some(help) = controls.help {
                let id = match target {
                    ShellWindow::Pane(Focus::Manager) => "help-files",
                    ShellWindow::Pane(Focus::Editor) => "help-editor",
                    ShellWindow::FilePicker => "help-file-picker",
                    ShellWindow::ShortcutHelp => unreachable!("help only has a close button"),
                };
                root = root.child(corner_button(help.pixels(self.metrics)).id(id).on_click(
                    cx.listener(move |workspace, _, window, cx| {
                        if workspace.hovered_controls != Some(target) {
                            return;
                        }
                        match target {
                            ShellWindow::Pane(pane) if !workspace.modal_open() => {
                                workspace.open_shortcut_help(pane, window, cx);
                            }
                            ShellWindow::FilePicker => workspace.open_file_picker_help(window, cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }),
                ));
            }
            let id = match target {
                ShellWindow::Pane(Focus::Manager) => "close-files",
                ShellWindow::Pane(Focus::Editor) => "close-editor",
                ShellWindow::FilePicker => "close-file-picker",
                ShellWindow::ShortcutHelp => "close-shortcuts",
            };
            root = root.child(
                corner_button(controls.close.pixels(self.metrics))
                    .id(id)
                    .on_click(cx.listener(move |workspace, event, window, cx| {
                        workspace.click_close_button(target, event, window, cx);
                    }))
                    .child(
                        canvas(
                            |_, _, _| (),
                            |bounds, _, window, cx| {
                                let half =
                                    point(px(CLOSE_ICON_SIZE / 2.), px(CLOSE_ICON_SIZE / 2.));
                                let center = bounds.center();
                                let icon = Bounds::from_corners(center - half, center + half);
                                cx.global::<IconCache>().paint(CLOSE_ICON, icon, window, cx);
                            },
                        )
                        .size_full(),
                    ),
            );
        }
        let view = cx.entity();
        root.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    view.update(cx, |workspace, cx| {
                        window.handle_input(
                            &workspace.focus_handle,
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
