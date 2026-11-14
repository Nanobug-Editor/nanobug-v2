//! Workspace adapter for presentation.

use super::*;

impl Workspace {
    /// The plugin manager and grid surface share one retained panel.
    pub(in crate::grid::workspace) fn plugin_panel(&self) -> &Entity<Panel> {
        &self.plugins.panel
    }

    /// Clip the base editor under document results so they can use the existing window material.
    pub(in crate::grid::workspace) fn document_results_area(&self) -> Option<Rect> {
        (self.plugins.open && (self.plugins.language.is_some() || self.plugins.outline.is_some()))
            .then(|| self.plugin_manager_area())
    }

    /// Return the area of the modal plugin popup that uses the shared frosted backdrop.
    pub(in crate::grid::workspace) fn plugin_popup_area(&self) -> Option<Rect> {
        (self.plugins.open && self.plugins.language.is_none() && self.plugins.outline.is_none())
            .then(|| self.plugin_manager_area())
    }

    /// Center secondary dialogs and fit the main plugin list inside the visible grid.
    pub(in crate::grid::workspace) fn plugin_manager_area(&self) -> Rect {
        if self.plugin_manager_view().uses_plugin_list_layout() {
            return ListLayout::area(
                self.layout.visible_cols,
                self.layout.visible_rows.min(self.layout.rows),
            );
        }
        let width = self.layout.cols.saturating_sub(4).min(94);
        let height = self.layout.rows.saturating_sub(3).clamp(MIN_HEIGHT, 25);
        Rect {
            x: self.layout.cols.saturating_sub(width) / 2,
            y: self.layout.rows.saturating_sub(height) / 2,
            w: width,
            h: height,
        }
    }

    /// Return the retained plugin panel, its grid bounds and the caret clip for the active input.
    pub(in crate::grid::workspace) fn plugin_cursor_panel(
        &self,
    ) -> Option<(Entity<Panel>, Rect, Option<gpui::Bounds<gpui::Pixels>>)> {
        let area = if self.plugins.open {
            self.plugin_manager_area()
        } else {
            self.plugin_surface_area()?
        };
        let field = if self.plugins.manager_accepts_text()
            && self.plugin_manager_view().uses_plugin_list_layout()
        {
            let footer = self.plugin_manager_view().plugin_list_layout(area).footer;
            Some(
                Rect {
                    x: area.x + footer.x,
                    y: area.y + footer.y,
                    ..footer
                }
                .pixels(self.metrics),
            )
        } else if self.plugins.surface.focused && !self.plugins.open {
            self.plugin_input_bounds(area)
        } else {
            None
        };
        Some((self.plugins.panel.clone(), area, field))
    }

    /// Resolve the focused plugin caret in grid cells with its width, insert mode and visibility.
    pub(in crate::grid::workspace) fn plugin_caret(&self) -> Option<(f32, f32, f32, bool, bool)> {
        if self.plugins.surface.focused && !self.plugins.open {
            let area = self.plugin_surface_area()?;
            if let Some(native) = self.canvas_input_caret_bounds() {
                return Some((
                    f32::from(native.origin.x - self.metrics.origin.x) / self.metrics.cw,
                    f32::from(native.origin.y - self.metrics.origin.y) / self.metrics.lh,
                    1.,
                    true,
                    true,
                ));
            }
            let (column, row, width) = self.input_cell(area);
            let units = self
                .plugins
                .surface
                .field
                .layout(
                    &self.plugins.surface.draft,
                    &self.plugins.composition,
                    width,
                    &self.spacing,
                )
                .caret;
            return Some((
                (area.x + column) as f32 + units as f32 / UNITS_PER_COLUMN as f32,
                (area.y + row) as f32,
                1.,
                true,
                true,
            ));
        }
        if !self.plugins.open {
            return None;
        }
        let area = self.plugin_manager_area();
        self.plugin_manager_view().caret(area)
    }

    /// Render the manager or the focused guest surface in `window` using retained panels from `cx`.
    pub(in crate::grid::workspace) fn plugin_element(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.plugins.open {
            return Some(self.plugin_manager_element(self.plugin_manager_area(), cx));
        }
        let area = self.plugin_surface_area()?;
        if self.update_plugin_viewport(area, window, cx) {
            self.update_cursor(window, cx);
        }
        self.schedule_canvas_frame(window, cx);
        let frame = self.plugins.surface.frame(
            area,
            &self.plugins.view,
            &self.plugins.composition,
            self.metrics,
            &self.spacing,
            self.current_native_field().is_some(),
        );
        let panel = self.plugins.panel.clone();
        panel.update(cx, |panel, cx| panel.set(frame, cx));
        let bounds = area.pixels(self.metrics);
        let input = self.plugin_surface_input_area(area);
        let report = self.canvas_underline_report(cx);
        Some(
            div()
                .absolute()
                .left(bounds.origin.x)
                .top(bounds.origin.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
                .overflow_hidden()
                .bg(rgba(style(cx).panel()))
                .children(
                    self.displayed_plugin_surface()
                        .and_then(|surface| {
                            canvas::element(
                                surface,
                                self.plugin_input_bounds(area),
                                report.map(|report| {
                                    Box::new(move |lines, window: &Window, cx: &mut gpui::App| {
                                        report.schedule(lines, window, cx)
                                    })
                                        as canvas::UnderlineReport
                                }),
                            )
                        })
                        .map(|canvas| canvas.absolute().left_0().top_0().size_full()),
                )
                .children(input.and_then(|input| self.plugin_selection(area, input, cx)))
                .children(self.canvas_text_element(area, style(cx)))
                .child(
                    panel.cached(
                        gpui::StyleRefinement::default()
                            .absolute()
                            .left_0()
                            .top_0()
                            .size_full(),
                    ),
                )
                .into_any_element(),
        )
    }

    /// Place the manager frame and its detail outline in `area`, using the retained panel in `cx`.
    pub(in crate::grid::workspace) fn plugin_manager_element(
        &self,
        area: Rect,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frame = self.plugin_manager_view().manager_frame(area);
        let panel = self.plugins.panel.clone();
        panel.update(cx, |panel, cx| panel.set(frame, cx));
        let manager = &self.plugins.manager;
        let description = if self.plugin_manager_view().uses_plugin_list_layout() {
            let layout = self.plugin_manager_view().plugin_list_layout(area);
            Some((layout.description, layout.description_title(&self.spacing)))
        } else if !manager.help
            && manager.confirm_remove.is_none()
            && self.plugins.view.review.is_none()
            && self.document_results_area().is_none()
        {
            Some((secondary_description(area), 0..0))
        } else {
            None
        };
        let outline = description
            .filter(|(rect, _)| rect.h >= 3 && rect.w >= 3)
            .map(|(rect, title)| {
                dashed_outline_element(
                    Rect {
                        x: area.x + rect.x,
                        y: area.y + rect.y,
                        ..rect
                    },
                    self.metrics,
                    title,
                )
            });
        div()
            .absolute()
            .left_0()
            .top_0()
            .size_full()
            .children(outline)
            .child(panel.cached(positioned(area.pixels(self.metrics))))
            .into_any_element()
    }

    /// Adapt operation results to the manager window without exposing workspace state to it.
    pub(in crate::grid::workspace) fn plugin_manager_view(&self) -> ManagerView<'_> {
        let ui = &self.plugins;
        let title = if ui.formatting.choices.is_some() {
            Some("Choose a formatter".into())
        } else if let Some(results) = &ui.language {
            Some(results.title().into())
        } else if ui.outline.is_some() {
            Some(i18n::text("editor.document_outline").into())
        } else if ui.file_plan.is_some() || ui.file_busy || !ui.file_report.is_empty() {
            Some("Plugin file operations".into())
        } else if ui.edit_plan.is_some() {
            Some("Plugin edit preview".into())
        } else if ui.close_draft.is_some() {
            Some("Save plugin inputs".into())
        } else {
            None
        };
        let document_hint = (ui.outline.is_some() || ui.language.is_some()).then(|| {
            if ui.outline.is_some()
                || ui
                    .language
                    .as_ref()
                    .is_some_and(|result| result.jumps_to_result())
            {
                "editor.document_jump_hint"
            } else {
                "editor.document_complete_hint"
            }
        });
        let bindings = if ui.manager.help {
            shortcuts::bindings(&ui.view, ui.active, self.plugin_command_context())
                .iter()
                .map(|binding| {
                    format!(
                        "{:<15}{}{}",
                        binding.shortcut,
                        binding.label,
                        binding
                            .issue
                            .map_or_else(String::new, |issue| format!(" · {issue}"))
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        ManagerView {
            manager: &ui.manager,
            data: ui.row_context(),
            composition: &ui.composition,
            spacing: &self.spacing,
            presentation: Presentation {
                title,
                rows: ui.has_operation_rows().then(|| workflow::rows(ui)),
                document_hint,
                empty_label: ui.language.as_ref().map_or_else(
                    || i18n::text("plugin_manager.empty"),
                    |results| results.empty_label(),
                ),
                bindings,
            },
        }
    }
}
