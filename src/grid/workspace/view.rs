//! Compose retained panels, the cursor and workspace overlays.

use crate::grid::windows::manager::ManagerWindow;

use crate::grid::render::{GridLayer, Panel, Rect, exposed_regions, popup_outline};
use crate::grid::windows::{help::HelpWindow, start::StartPage};

use super::{
    Workspace,
    language::language_list_bounds,
    layout::{Focus, ShellWindow},
    leader_hint::border_gaps,
};
use crate::ui::{
    icons::{Icon, IconCache},
    theme::style,
};
#[cfg(windows)]
use gpui::WindowControlArea;
use gpui::{
    AnyElement, Bounds, Context, ElementInputHandler, Entity, InteractiveElement, IntoElement,
    MouseButton, NavigationDirection, ParentElement, Pixels, Render, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, canvas, div, point, px, rgba, size,
};
use nanobug::animation::scroll::ScrollInput;

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
        let [numbers, text] = project.editor.content_layers(
            self.layout.editor,
            self.metrics,
            &self.bodies[1],
            &self.bodies[2],
        );
        Some([
            fixed(&self.panels[0], self.layout.tree),
            fixed(&self.panels[1], self.layout.editor),
            GridLayer {
                panel: self.panels[2].clone(),
                bounds: self.footer_bounds(),
                clip: None,
            },
            scrolled(
                &self.bodies[0],
                ManagerWindow::body(self.layout.tree),
                project
                    .manager
                    .as_ref()
                    .map_or(0., |manager| manager.scroll().fraction()),
            ),
            numbers,
            text,
        ])
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
fn layer_element(
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
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_exit(
                cx.listener(|workspace, _, _, cx| workspace.set_controls_hover(None, cx)),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll));
        if self.has_canvas_button_callbacks() {
            root = root.on_mouse_up(MouseButton::Middle, cx.listener(Self::plugin_canvas_up));
            for button in [
                MouseButton::Right,
                MouseButton::Navigate(NavigationDirection::Back),
                MouseButton::Navigate(NavigationDirection::Forward),
            ] {
                root = root
                    .on_mouse_down(button, cx.listener(Self::plugin_canvas_aux_down))
                    .on_mouse_up(button, cx.listener(Self::plugin_canvas_up));
            }
        }
        let layers = self
            .file_picker_layers()
            .or_else(|| self.grid_layers().map(Vec::from));
        let start_layout = self
            .file_picker
            .is_none()
            .then(|| self.start_layout())
            .flatten();
        let start = start_layout.map(|layout| GridLayer {
            panel: self.start_panel.clone(),
            bounds: layout
                .frame_area()
                .pixels(StartPage::content_metrics(self.metrics)),
            clip: None,
        });
        let confirmation = self
            .editor_close_area()
            .or_else(|| self.remove_recent_project_area());
        let language_picker = self.language_picker_area();
        let picker_prompt = self.file_picker_prompt_area();
        let document_results = self.document_results_area();
        let plugin_popup = self.plugin_popup_area();
        let small_modal = document_results
            .or(confirmation)
            .or(language_picker)
            .or(picker_prompt);
        let small_popup = if self.help.is_open() {
            None
        } else {
            small_modal.or_else(|| self.sequence_hint_area())
        };
        let message = if self.help.is_open() {
            None
        } else {
            self.file_picker_message_layer()
        };
        let small_popup_bounds = small_popup.map(|area| area.pixels(self.metrics));
        let leader_bounds = small_popup_bounds.filter(|_| small_modal.is_none());
        let message_bounds = message.as_ref().map(|layer| layer.bounds);
        if let Some(area) = plugin_popup {
            let outline = popup_outline(area.pixels(self.metrics), self.metrics);
            root = root.child(self.popup_backdrop.element(
                layers.iter().flatten().chain(&start),
                outline,
                self.glyphs.as_ref(),
                window,
                cx,
            ));
        } else if let Some(list) = &self.help.list {
            let area = self.help_layout();
            let grid = area.pixels(self.metrics);
            let outline = popup_outline(grid, self.metrics);
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
            let backdrop = self.popup_backdrop.element(
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
            self.popup_backdrop.release(window);
            let preview = if self.file_picker.is_none() && layers.is_some() {
                self.tree_name_preview_layer()
            } else {
                None
            };
            if small_popup_bounds.is_some() || message_bounds.is_some() {
                let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                let popup_cuts = leader_bounds.map_or_else(
                    || small_popup_bounds.into_iter().collect(),
                    |popup| self.leader_cutouts(popup, start_layout),
                );
                let regions =
                    visible_regions(viewport, popup_cuts.into_iter().chain(message_bounds));
                // Give each clip its own cached-view scope for the shared panels and cursor.
                for (index, region) in regions.into_iter().enumerate() {
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
                            layer_element(
                                GridLayer {
                                    panel: layer.panel.clone(),
                                    bounds: layer.bounds,
                                    clip: Some(clip),
                                },
                                window,
                            )
                        });
                    let mut underlay = div()
                        .id(("popup-underlay", index))
                        .absolute()
                        .left_0()
                        .top_0()
                        .size_full()
                        .children(visible);
                    if !self.small_modal_open()
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
                    let cuts =
                        small_popup_bounds
                            .into_iter()
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
                if let Some(area) = small_popup.filter(|_| document_results.is_none()) {
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
                root = root.children(
                    layers
                        .into_iter()
                        .flatten()
                        .chain(start)
                        .chain(preview)
                        .filter_map(|layer| layer_element(layer, window)),
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
        if let Some(area) = small_popup.filter(|_| small_modal.is_none()) {
            root = root.child(self.sequence_hint_element(area, cx));
        }
        if plugin_popup.is_some()
            || confirmation.is_none()
                && (language_picker.is_some()
                    || picker_prompt.is_some()
                    || document_results.is_some()
                    || small_popup.is_none() && message_bounds.is_none())
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
            // The whole project strip participates in the native caption move loop.
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
            root = root.children(self.tabs.click_targets(tab_metrics).map(|(index, bounds)| {
                div()
                    .id(("project-tab", index))
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.activate_project_tab(index, window, cx);
                        cx.stop_propagation();
                    }))
            }));
        }
        if let Some(target) = self.hovered_controls
            && target != ShellWindow::Pane(Focus::Editor)
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
                    ShellWindow::Pane(Focus::Editor) => unreachable!("editor uses its title row"),
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
                ShellWindow::Pane(Focus::Editor) => unreachable!("editor uses its title row"),
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
