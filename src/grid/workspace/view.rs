//! Compose retained panels, the cursor and workspace overlays.

use crate::grid::windows::manager::ManagerWindow;
use crate::ui::components::grid_dialog::DialogBackdrop;

use crate::grid::render::{GridLayer, Panel, Rect, dialog_backdrop, exposed_regions};
use crate::grid::windows::{
    editor::{CompletionMenu, DefinitionHoverPopup},
    help::HelpWindow,
};

use super::{
    Workspace,
    editor_splits::SplitAxis,
    language::language_list_bounds,
    layout::{ShellWindow, stacked_divider_bounds},
    leader_hint::border_gaps,
    markdown::external_markdown_url,
    pointer::PaneResize,
};
use crate::ui::{
    icons::{Icon, IconCache},
    theme::{ThemeComponent, style},
};
#[cfg(windows)]
use gpui::WindowControlArea;
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, CursorStyle, ElementInputHandler, Entity,
    InteractiveElement, IntoElement, MouseButton, NavigationDirection, ParentElement, Pixels,
    Render, StatefulInteractiveElement, StyleRefinement, Styled, Window, canvas, div, point, px,
    rgba, size,
};
use nanobug::animation::scroll::ScrollInput;
use std::rc::Rc;

/// Cross drawn in the border gap for a hovered popup.
const CLOSE_ICON: Icon = Icon::new(include_bytes!("../../../assets/icons/pane-close.svg"), 1.);
/// The close icon's square size in logical pixels, excluding the clickable padding.
const CLOSE_ICON_SIZE: f32 = 12.;

/// Map the resized grid dimension to the corresponding native cursor.
fn divider_cursor(axis: SplitAxis) -> CursorStyle {
    match axis {
        SplitAxis::Columns => CursorStyle::ResizeLeftRight,
        SplitAxis::Rows => CursorStyle::ResizeUpDown,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::layout::Focus;
    use anyhow::{Context as _, ensure};
    use gpui::AppContext;
    use nanobug::plugins::session::Options;

    /// Compare rendered pixels across the Explorer edge, including the covered border.
    #[test]
    #[ignore = "requires Windows DirectWrite and DirectX"]
    fn native_overflowing_tree_name_has_a_continuous_background() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        std::fs::create_dir(&project)?;
        let filename = format!("{}.txt", "long-file-name-".repeat(7));
        std::fs::write(project.join(filename), "")?;
        let options = Options::take(
            &mut vec![
                "--plugin-profile".into(),
                temp.path().join("profile").into_os_string(),
                "--no-plugins".into(),
            ],
            temp.path(),
        )?;
        super::super::native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Manager),
            async move |handle, app| {
                let _keep_project = temp;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(size(px(920.), px(580.)))
                })?;
                app.background_executor()
                    .timer(std::time::Duration::from_millis(60))
                    .await;
                let (left, right, y) = app.update(|app| {
                    handle.update(app, |workspace, window, cx| -> anyhow::Result<_> {
                        workspace.sync(window, cx);
                        let preview = workspace
                            .tree_name_preview_layer()
                            .context("long name preview")?;
                        let row = preview.clip.context("visible selection row")?;
                        let edge = ManagerWindow::body(workspace.layout.tree)
                            .pixels(workspace.metrics)
                            .right();
                        let distance = px(workspace.metrics.cw);
                        ensure!(row.left() < edge - distance && row.right() > edge + distance);
                        let device =
                            |value| (f32::from(value) * window.scale_factor()).round() as u32;
                        let left = device(edge - distance);
                        let right = device(edge + distance);
                        let y = device(row.top()) + 1;
                        Ok((left, right, y))
                    })
                })??;
                let image = app.update_window(handle.into(), |_, window, cx| {
                    window.draw(cx).clear(cx);
                    window.render_to_image()
                })??;
                ensure!(right < image.width() && y < image.height());
                let expected = image.get_pixel(left, y);
                for x in left + 1..=right {
                    ensure!(
                        image.get_pixel(x, y) == expected,
                        "background changes at x={x}: {expected:?} vs {:?}",
                        image.get_pixel(x, y),
                    );
                }
                Ok(())
            },
        )
    }
}

impl Workspace {
    /// Route external links from the painted `popup` in `window` through the workspace `cx`.
    fn definition_hover_links(
        &self,
        popup: DefinitionHoverPopup,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_height = self.metrics.lh;
        let bounds = popup.body_bounds();
        let popup = Rc::new(popup);
        let link = move |position| {
            popup
                .link_at(position, row_height)
                .and_then(external_markdown_url)
        };
        let hot = link(window.mouse_position()).is_some();
        let move_link = link.clone();
        let press_link = link.clone();
        let layer = div()
            .id("definition-hover-links")
            .absolute()
            .left(bounds.left())
            .top(bounds.top())
            .w(bounds.size.width)
            .h(bounds.size.height)
            .on_mouse_move(cx.listener(move |_, event: &gpui::MouseMoveEvent, _, cx| {
                if move_link(event.position).is_some() != hot {
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                if press_link(event.position).is_some() {
                    cx.stop_propagation();
                }
            })
            .on_click(cx.listener(move |workspace, event, window, cx| {
                if let ClickEvent::Mouse(click) = event
                    && click.down.click_count == 1
                    && let Some(url) = link(click.down.position)
                    && workspace
                        .definition_hover_popup(window, cx)
                        .and_then(|current| {
                            current
                                .link_at(click.up.position, row_height)
                                .and_then(external_markdown_url)
                        })
                        .as_ref()
                        == Some(&url)
                {
                    cx.open_url(url.as_str());
                    cx.stop_propagation();
                }
            }));
        if hot { layer.cursor_pointer() } else { layer }.into_any_element()
    }

    /// Pair ready completion labels with the menu fitted inside `window` at the logical caret.
    pub(super) fn completion_menu(
        &self,
        window: &Window,
    ) -> Option<(CompletionMenu, Vec<&str>, usize)> {
        let (labels, selected) = self.completion_items()?;
        let editor = &self.projects.active_project()?.editor;
        let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
        let mut menu = editor.completion_menu(
            self.active_editor_area(),
            self.metrics,
            &self.spacing,
            viewport,
            &labels,
            selected,
        )?;
        self.position_completion_menu(&mut menu, labels.len(), selected, false);
        Some((menu, labels, selected))
    }

    /// Place fixed panels and scrollable bodies, keeping the footer aligned to the bottom inset.
    fn grid_layers(&self) -> Option<Vec<GridLayer>> {
        let project = self.projects.active_project()?;
        let fixed = |panel: &Entity<Panel>, rect: Rect| GridLayer {
            panel: panel.clone(),
            bounds: rect.pixels(self.metrics),
            clip: None,
        };
        let scrolled =
            |panel, rect, fraction| GridLayer::scrolled(panel, rect, fraction, self.metrics);
        let editor_area = self.active_editor_area();
        let [numbers, text] = project.editor.content_layers(
            editor_area,
            self.metrics,
            &self.editor_panels.numbers,
            &self.editor_panels.text,
        );
        let mut layers = vec![
            fixed(&self.manager_chrome, self.layout.tree),
            fixed(&self.editor_panels.chrome, editor_area),
            GridLayer {
                panel: self.footer_panel.clone(),
                bounds: self.footer_bounds(),
                clip: None,
            },
            scrolled(
                &self.manager_body,
                ManagerWindow::body(self.layout.tree),
                project
                    .manager
                    .as_ref()
                    .map_or(0., |manager| manager.scroll().fraction()),
            ),
            numbers,
            text,
        ];
        for (pane, area) in self.editor_rects() {
            let Some(panels) = self
                .inactive_editor_panels
                .iter()
                .find(|entry| entry.pane == pane)
            else {
                continue;
            };
            let Some(editor) = project.editor_by_id(pane) else {
                continue;
            };
            layers.push(fixed(&panels.layers.chrome, area));
            layers.extend(editor.content_layers(
                area,
                self.metrics,
                &panels.layers.numbers,
                &panels.layers.text,
            ));
        }
        Some(layers)
    }

    /// Match source text panels that must clip around an inline popup.
    fn is_editor_text_panel(&self, panel: &Entity<Panel>) -> bool {
        panel == &self.editor_panels.text
            || self
                .inactive_editor_panels
                .iter()
                .any(|editor| panel == &editor.layers.text)
    }

    /// Position the preview over its scrolled tree row, clipping vertically to the tree body.
    fn tree_name_preview_layer(&self) -> Option<GridLayer> {
        let mut bounds = self.tree_name_preview_rect?.pixels(self.metrics);
        let manager = self.projects.active().manager.as_ref()?;
        bounds.origin.y -= px(manager.scroll().fraction() * self.metrics.lh);
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
pub(super) fn layer_element(
    GridLayer {
        panel,
        bounds,
        clip,
    }: GridLayer,
    window: &Window,
) -> Option<AnyElement> {
    if bounds.is_empty() {
        return None;
    }
    Some(if let Some(clip) = clip {
        clipped_entity(panel, bounds, clip, window).into_any_element()
    } else {
        panel.cached(positioned(bounds)).into_any_element()
    })
}

/// Exclude `popup` from a retained `layer` while painting it in `window`.
/// The clipped strips have separate cache scopes inside this layer.
fn layer_around_popup(
    layer: GridLayer,
    popup: Option<Bounds<Pixels>>,
    window: &Window,
) -> Option<AnyElement> {
    let clip = layer.clip.unwrap_or(layer.bounds);
    let Some(popup) = popup.filter(|bounds| !clip.intersect(bounds).is_empty()) else {
        return layer_element(layer, window);
    };
    Some(
        div()
            .id(("popup-layer-underlay", layer.panel.entity_id()))
            .absolute()
            .left_0()
            .top_0()
            .size_full()
            .children(
                exposed_regions(clip, popup)
                    .into_iter()
                    .enumerate()
                    .filter_map(|(side, clip)| {
                        if clip.is_empty() {
                            return None;
                        }
                        Some(
                            div()
                                .id(("inline-popup-text-strip", side))
                                .absolute()
                                .left_0()
                                .top_0()
                                .size_full()
                                .child(clipped_entity(
                                    layer.panel.clone(),
                                    layer.bounds,
                                    clip,
                                    window,
                                )),
                        )
                    }),
            )
            .into_any_element(),
    )
}

/// Draw `entity` at its window-relative `bounds` through a fixed rectangular `clip`.
/// Use `window` to align both origins before computing the relative inset.
fn clipped_entity<V: Render>(
    entity: Entity<V>,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    window: &Window,
) -> impl IntoElement {
    // GPUI snaps parent and child insets independently. Subtract only after snapping
    // their absolute origins, so a different clip cannot move the content by a pixel.
    let clip = Bounds::from_corners(
        window.pixel_snap_point(clip.origin),
        window.pixel_snap_point(clip.bottom_right()),
    );
    let local = Bounds::new(
        window.pixel_snap_point(bounds.origin) - clip.origin,
        bounds.size,
    );
    div()
        .absolute()
        .left(clip.origin.x)
        .top(clip.origin.y)
        .w(clip.size.width)
        .h(clip.size.height)
        .overflow_hidden()
        .child(entity.cached(positioned(local)))
}

/// Keep the portions of `viewport` outside the overlay `cutouts`.
fn visible_regions(
    viewport: Bounds<Pixels>,
    cutouts: impl IntoIterator<Item = Bounds<Pixels>>,
) -> Vec<Bounds<Pixels>> {
    cutouts.into_iter().fold(vec![viewport], |visible, cut| {
        visible
            .into_iter()
            .flat_map(|region| exposed_regions(region, cut))
            .filter(|region| !region.is_empty())
            .collect()
    })
}

impl Render for Workspace {
    /// Compose the workspace with the active popup, if any.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.update_display_scale(window, cx) || self.derive_layout(window) != self.layout {
            self.sync(window, cx);
        }
        if !window.is_window_hovered() {
            self.set_controls_hover(None, cx);
        }
        let mut root = div()
            .id("nanobug")
            .relative()
            .size_full()
            .bg(rgba(style(cx).background()))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_modifiers_changed(cx.listener(
                |workspace, event: &gpui::ModifiersChangedEvent, window, cx| {
                    if workspace
                        .help
                        .update_keyboard_modifiers(event.modifiers, window.is_window_active())
                    {
                        workspace
                            .cursor
                            .update(cx, |cursor, cx| cursor.activity(window, cx));
                        workspace.repaint_help(ScrollInput::Advance, window, cx);
                    }
                },
            ))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::middle_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::right_mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_exit(cx.listener(|workspace, _, _, cx| {
                workspace.set_controls_hover(None, cx);
                workspace.update_markdown_pointer(None, cx);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll));
        if let Some(PaneResize::Editor(divider, _)) = self.pane_resize {
            root = root.cursor(divider_cursor(divider.axis));
        } else {
            root = root.children(self.editor_dividers().into_iter().map(|(axis, area)| {
                let bounds = if axis == SplitAxis::Rows {
                    stacked_divider_bounds(area, self.metrics)
                } else {
                    area.pixels(self.metrics)
                };
                div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .cursor(divider_cursor(axis))
            }));
        }
        if self.settings.is_none() && self.has_canvas_button_callbacks() {
            root = root
                .on_mouse_up(MouseButton::Middle, cx.listener(Self::plugin_canvas_up))
                .on_mouse_up(MouseButton::Right, cx.listener(Self::plugin_canvas_up));
            for button in [
                MouseButton::Navigate(NavigationDirection::Back),
                MouseButton::Navigate(NavigationDirection::Forward),
            ] {
                root = root
                    .on_mouse_down(button, cx.listener(Self::plugin_canvas_aux_down))
                    .on_mouse_up(button, cx.listener(Self::plugin_canvas_up));
            }
        }
        let layers = self.file_picker_layers().or_else(|| self.grid_layers());
        let start_layout = self
            .file_picker
            .is_none()
            .then(|| self.start_layout())
            .flatten();
        let start = self.start_layers(start_layout);
        let confirmation = self
            .editor_close_area()
            .or_else(|| self.remove_recent_project_area());
        let language_picker = self.language_picker_area();
        let picker_prompt = self.file_picker_prompt_area();
        let document_results = self.document_results_area();
        let plugin_popup = self.plugin_popup_area();
        let command_window = self.command_window_area();
        let vim_menu = self.vim_menu_area();
        let small_modal = document_results
            .or(confirmation)
            .or(language_picker)
            .or(picker_prompt)
            .or(command_window)
            .or(vim_menu);
        let sequence_hint = self.sequence_hint_area();
        let small_popup = (!self.help.is_open())
            .then(|| small_modal.or(sequence_hint))
            .flatten();
        let message = if self.help.is_open() {
            None
        } else {
            self.file_picker_message_layer()
        };
        let small_popup_bounds = small_popup.map(|area| area.pixels(self.metrics));
        let settings_bounds = self
            .settings
            .as_ref()
            .map(|_| self.settings_area().pixels(self.metrics));
        let leader_bounds = sequence_hint.map(|area| area.pixels(self.metrics));
        let modal_bounds = small_popup_bounds.filter(|_| small_modal.is_some());
        let message_bounds = message.as_ref().map(|layer| layer.bounds);
        let (completion_bounds, completion) =
            self.completion_menu(window)
                .map_or((None, None), |(mut menu, labels, selected)| {
                    self.position_completion_menu(&mut menu, labels.len(), selected, true);
                    let bounds = menu.bounds();
                    let element = menu.element(
                        &labels,
                        self.completion_query(),
                        selected,
                        self.metrics,
                        &self.spacing,
                    );
                    (Some(bounds), Some(element))
                });
        if self.completion_scrolling() {
            self.schedule_scroll(window, cx);
        }
        let hover = if small_popup.is_none() && plugin_popup.is_none() {
            self.definition_hover_popup(window, cx)
        } else {
            None
        };
        let inline_popup_bounds =
            completion_bounds.or_else(|| hover.as_ref().map(|popup| popup.bounds()));
        let markdown =
            if self.file_picker.is_none() && !self.help.is_open() && plugin_popup.is_none() {
                self.prepare_markdown_previews(window, cx)
            } else {
                Vec::new()
            };
        if let Some(area) = plugin_popup {
            let outline = DialogBackdrop::outline(
                area.pixels(self.metrics),
                self.metrics.cw,
                self.metrics.lh,
            );
            root = root.child(dialog_backdrop(
                &mut self.dialog_backdrop,
                layers.iter().flatten().chain(&start),
                outline,
                self.glyphs.as_ref(),
                window,
                cx,
            ));
        } else if let Some(list) = &self.help.list {
            let area = self.help_layout();
            let grid = area.pixels(self.metrics);
            let outline = DialogBackdrop::outline(grid, self.metrics.cw, self.metrics.lh);
            let filtering = list.is_filtering();
            let body = GridLayer::scrolled(
                &self.help.body,
                self.help.body(area),
                if self.help.keyboard_visible() {
                    0.
                } else {
                    list.scroll.fraction()
                },
                HelpWindow::content_metrics(self.metrics),
            );
            let tab_targets = list.tab_targets(
                area,
                self.metrics,
                &self.spacing,
                self.hovered_controls == Some(ShellWindow::ShortcutHelp),
            );
            let backdrop = dialog_backdrop(
                &mut self.dialog_backdrop,
                layers.iter().flatten(),
                outline,
                self.glyphs.as_ref(),
                window,
                cx,
            );
            root = root
                .child(backdrop)
                .child(self.help.panel.clone().cached(positioned(grid)))
                .children(self.help.keyboard_element(area, self.metrics))
                .children(layer_element(body, window))
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
            let help_button = |bounds: Bounds<Pixels>| {
                div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            };
            for (index, (scope, bounds)) in tab_targets
                .into_iter()
                .enumerate()
                .filter(|(_, (_, bounds))| !bounds.is_empty())
            {
                root = root.child(help_button(bounds).id(("help-tab", index)).on_click(
                    cx.listener(move |workspace, _, window, cx| {
                        let now = workspace.scroll_epoch.elapsed().as_secs_f64();
                        if workspace.help.select_scope(scope, now) {
                            workspace
                                .cursor
                                .update(cx, |cursor, cx| cursor.activity(window, cx));
                            workspace.repaint_help(ScrollInput::Advance, window, cx);
                        }
                        cx.stop_propagation();
                    }),
                ));
            }
            if filtering {
                let search = HelpWindow::search_bounds(area, self.metrics);
                root = root.child(self.help.search.clone().cached(positioned(search)));
                let [apply, cancel] = HelpWindow::search_buttons(area, self.metrics, &self.spacing);
                for (id, bounds, cancel) in [
                    ("help-search-apply", apply, false),
                    ("help-search-cancel", cancel, true),
                ] {
                    root = root.child(help_button(bounds).id(id).on_click(cx.listener(
                        move |workspace, _, window, cx| {
                            workspace.finish_help_filter(cancel, window, cx);
                            cx.stop_propagation();
                        },
                    )));
                }
            }
        } else {
            self.dialog_backdrop.release(window);
            let preview = if self.file_picker.is_none() && layers.is_some() {
                self.tree_name_preview_layer()
            } else {
                None
            };
            let preview_extension = preview.as_ref().and_then(|preview| {
                let clip = preview.clip?;
                let tree_edge = ManagerWindow::body(self.layout.tree)
                    .pixels(self.metrics)
                    .right();
                (clip.right() > tree_edge).then(|| {
                    Bounds::from_corners(point(tree_edge, clip.top()), clip.bottom_right())
                })
            });
            let clip_popups = small_popup_bounds.is_some()
                || message_bounds.is_some()
                || settings_bounds.is_some();
            if clip_popups || preview_extension.is_some() {
                let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                let popup_cuts = leader_bounds
                    .into_iter()
                    .flat_map(|popup| self.leader_cutouts(popup, start_layout))
                    .chain(modal_bounds)
                    .chain(settings_bounds);
                let regions = visible_regions(viewport, popup_cuts.chain(message_bounds));
                // Give each clip its own cached-view scope for the shared panels and cursor.
                for (index, region) in regions.into_iter().enumerate() {
                    // Expose the window material under the extension so it composites like the tree row.
                    let base = visible_regions(region, preview_extension)
                        .into_iter()
                        .enumerate()
                        .map(|(strip, region)| {
                            let visible =
                                layers.iter().flatten().chain(&start).filter_map(|layer| {
                                    let clip =
                                        region.intersect(&layer.clip.unwrap_or(layer.bounds));
                                    if clip.is_empty() {
                                        return None;
                                    }
                                    let layer = GridLayer {
                                        panel: layer.panel.clone(),
                                        bounds: layer.bounds,
                                        clip: Some(clip),
                                    };
                                    if self.is_editor_text_panel(&layer.panel) {
                                        layer_around_popup(layer, inline_popup_bounds, window)
                                    } else {
                                        layer_element(layer, window)
                                    }
                                });
                            div()
                                .id(("popup-underlay-strip", strip))
                                .absolute()
                                .left_0()
                                .top_0()
                                .size_full()
                                .children(visible)
                                .children(
                                    markdown
                                        .iter()
                                        .filter_map(|(_, preview)| preview.element(Some(region))),
                                )
                        });
                    let mut underlay = div()
                        .id(("popup-underlay", index))
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full()
                        .children(base)
                        .children(preview.iter().filter_map(|preview| {
                            let clip = region.intersect(&preview.clip.unwrap_or(preview.bounds));
                            (!clip.is_empty()).then(|| {
                                clipped_entity(preview.panel.clone(), preview.bounds, clip, window)
                            })
                        }));
                    if clip_popups
                        && !self.small_modal_open()
                        && picker_prompt.is_none()
                        && document_results.is_none()
                    {
                        underlay = underlay.child(clipped_entity(
                            self.cursor.clone(),
                            viewport,
                            region,
                            window,
                        ));
                    }
                    root = root.child(underlay);
                }
                if let Some(message) = message {
                    // An interactive popup can cover part of the message while the timer runs.
                    let cuts = modal_bounds
                        .into_iter()
                        .chain(settings_bounds)
                        .chain(leader_bounds)
                        .chain(inline_popup_bounds)
                        .chain(leader_bounds.into_iter().flat_map(|popup| {
                            border_gaps(message.bounds, popup, self.metrics, self.metrics)
                        }));
                    for (index, clip) in visible_regions(message.bounds, cuts)
                        .into_iter()
                        .enumerate()
                    {
                        root = root.child(
                            div()
                                .id(("message-underlay", index))
                                .absolute()
                                .left_0()
                                .top_0()
                                .size_full()
                                .child(clipped_entity(
                                    message.panel.clone(),
                                    message.bounds,
                                    clip,
                                    window,
                                )),
                        );
                    }
                }
                if let Some((menu, area)) = self.vim_menu.as_ref().zip(vim_menu) {
                    root = root.children(
                        menu.layers(area, self.metrics)
                            .into_iter()
                            .filter_map(|layer| layer_element(layer, window)),
                    );
                } else if let Some((history, area)) =
                    self.command_window.as_ref().zip(command_window)
                {
                    root = root.children(
                        history
                            .layers(area, self.metrics)
                            .into_iter()
                            .filter_map(|layer| layer_around_popup(layer, leader_bounds, window)),
                    );
                } else if let Some(area) = small_popup.filter(|_| document_results.is_none()) {
                    if self.small_modal_open() || picker_prompt.is_some() {
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
                    }
                }
            } else {
                root = root
                    .children(
                        layers
                            .into_iter()
                            .flatten()
                            .chain(start)
                            .chain(preview)
                            .filter_map(|layer| {
                                if self.is_editor_text_panel(&layer.panel) {
                                    layer_around_popup(layer, inline_popup_bounds, window)
                                } else {
                                    layer_element(layer, window)
                                }
                            }),
                    )
                    .children(
                        markdown
                            .iter()
                            .filter_map(|(_, preview)| preview.element(None)),
                    );
            }
        }
        #[cfg(feature = "plugin-probe")]
        {
            root = root.children(
                self.probe_layer(window, cx)
                    .and_then(|layer| layer_element(layer, window)),
            );
            self.probe_after_frame(window, cx);
        }
        let tab_metrics = self.tab_metrics();
        root = root.child(self.tabs.element(tab_metrics, window, cx));
        root = root.children(self.plugin_element(window, cx));
        self.plugin_after_frame(window, cx);
        if let Some(area) = sequence_hint {
            root = root.child(self.sequence_hint_element(area, cx));
        }
        if self.settings.is_none()
            && (plugin_popup.is_some()
                || confirmation.is_none()
                    && (language_picker.is_some()
                        || picker_prompt.is_some()
                        || document_results.is_some()
                        || command_window.is_some()
                        || small_popup.is_none() && message_bounds.is_none()))
        {
            let history_hint = leader_bounds.filter(|hint| {
                command_window
                    .is_some_and(|area| !area.pixels(self.metrics).intersect(hint).is_empty())
            });
            if let Some(hint) = history_hint {
                let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                for (index, region) in visible_regions(viewport, [hint]).into_iter().enumerate() {
                    root = root.child(
                        div()
                            .id(("history-hint-cursor", index))
                            .absolute()
                            .left_0()
                            .top_0()
                            .size_full()
                            .child(clipped_entity(
                                self.cursor.clone(),
                                viewport,
                                region,
                                window,
                            )),
                    );
                }
            } else {
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
        }
        root = root.children(completion);
        if let Some(popup) = hover {
            self.popup_panel.update(cx, |panel, cx| {
                panel.set(popup.footer_frame(&self.spacing), cx)
            });
            root = root
                .child(popup.chrome_element(self.metrics))
                .child(
                    self.popup_panel
                        .clone()
                        .cached(positioned(popup.footer_bounds(self.metrics.lh))),
                )
                .child(popup.body_element(self.metrics.lh))
                .child(self.definition_hover_links(popup, window, cx));
        }
        #[cfg(windows)]
        {
            // Both tab groups participate in the native caption move loop.
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
        #[cfg(not(windows))]
        if !self.modal_open() {
            use super::tabs::CaptionTarget;

            root = root.children(
                self.tabs
                    .click_targets(tab_metrics)
                    .map(|(target, bounds)| {
                        let id = match target {
                            CaptionTarget::Project(index) => ("project-tab", index),
                            CaptionTarget::Page(index) => ("tabpage", index),
                        };
                        div()
                            .id(id)
                            .absolute()
                            .left(bounds.origin.x)
                            .top(bounds.origin.y)
                            .w(bounds.size.width)
                            .h(bounds.size.height)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                workspace.activate_caption_target(target, window, cx);
                                cx.stop_propagation();
                            }))
                    }),
            );
        }
        if self.hovered_controls == Some(ShellWindow::ShortcutHelp)
            && let Some(controls) = self.help_layout().corner_controls(false)
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
            root = root.child(
                corner_button(controls.close.pixels(self.metrics))
                    .id("close-shortcuts")
                    .on_click(cx.listener(|workspace, event, window, cx| {
                        workspace.click_close_button(ShellWindow::ShortcutHelp, event, window, cx);
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
        let plugin_surface = self
            .plugin_surface_area()
            .map(|area| area.pixels(self.metrics));
        if self.set_painted_markdown_preview(
            markdown,
            [small_popup_bounds, inline_popup_bounds, plugin_surface],
            window,
        ) {
            root = root.cursor_pointer();
        }
        if let Some(settings) = &self.settings {
            let area = self.settings_area();
            let bounds = area.pixels(self.metrics);
            let selector = settings.selector_layers(area, self.metrics);
            let selector_bounds = selector.as_ref().map(|(bounds, _)| *bounds);
            let overlay = div()
                .id("settings-overlay")
                .absolute()
                .left_0()
                .top_0()
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                .on_scroll(cx.listener(Self::scroll))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |viewport, _, window, cx| {
                            // Leave the same window material exposed as behind the status bar.
                            for strip in exposed_regions(viewport, bounds) {
                                if !strip.is_empty() {
                                    window.paint_quad(gpui::fill(strip, rgba(0x050a1480)));
                                }
                            }
                            if let Some(tint) = style(cx)
                                .for_component(ThemeComponent::Footer)
                                .component_background()
                            {
                                window.paint_quad(gpui::fill(bounds, rgba(tint)));
                            }
                        },
                    )
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full(),
                )
                .children(
                    settings
                        .layers(area, self.metrics)
                        .into_iter()
                        .filter_map(|layer| layer_around_popup(layer, selector_bounds, window)),
                )
                .children(
                    selector
                        .into_iter()
                        .flat_map(|(_, layers)| layers)
                        .filter_map(|layer| layer_element(layer, window)),
                );
            let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
            root = root.child(overlay.child(clipped_entity(
                self.cursor.clone(),
                viewport,
                bounds,
                window,
            )));
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
                        );
                        workspace.trace_document_scene();
                        workspace.retire_canvas_images_after_paint(window, cx);
                        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
                        crate::ui::native_atlas::finish_frame(window, cx);
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
