//! Connect the grid settings editor to its configuration owners and native input routes.

mod core;
mod cursor;
#[cfg(all(test, windows))]
mod native_tests;
mod plugins;
mod transaction;
pub(super) use core::CoreBaseline;
pub(super) use cursor::{CursorBaseline, saved_cursor_effect};

use super::{Workspace, viewport::CursorSpace};
use crate::grid::{
    render::Rect,
    windows::settings::{Action, Settings},
};
use gpui::{App, Context, KeyDownEvent, Pixels, Point, ScrollEvent, Window, px};

impl Workspace {
    /// Size the foreground dialog against the visible grid, including when the window is narrow.
    pub(super) fn settings_area(&self) -> Rect {
        Settings::area(self.layout.visible_region(self.layout.pane_area()))
    }

    /// Capture each setting from its live owner before permitting edits.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        self.dismiss_completion(cx);
        self.dismiss_definition_hover(cx);
        self.dismiss_sequence_hint(cx);
        self.pane_resize = None;
        self.prefix = None;
        self.settings_generation = self.settings_generation.wrapping_add(1);
        self.settings_catalog_started = false;
        let mut pages = Vec::new();
        let mut errors = Vec::new();
        match self
            .capture_core_settings_baseline()
            .and_then(|_| self.core_settings_pages())
        {
            Ok(core) => pages.extend(core),
            Err(error) => errors.push(format!("{error:#}")),
        }
        match self.capture_cursor_settings(cx) {
            Ok(()) => pages.extend(self.cursor_settings_pages()),
            Err(error) => errors.push(format!("{error:#}")),
        }
        let mut settings = Settings::new(pages, self.metrics, cx);
        settings.set_error(errors.join(" · "));
        self.settings = Some(settings);
        self.begin_plugin_catalog(window, cx);
        self.sync(window, cx);
    }

    /// Text and IME candidates use native input. Navigation and mnemonics stay inside the dialog.
    pub(super) fn settings_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_writing {
            cx.stop_propagation();
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        let area = self.settings_area();
        let settings = self.settings.as_mut().unwrap();
        let key = &event.keystroke;
        if settings.accepts_text() {
            if !settings.composition.is_empty() {
                return;
            }
            if (key.modifiers.control || key.modifiers.platform) && key.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    settings.insert_text(&text);
                }
                self.repaint_settings(window, cx);
                cx.stop_propagation();
                return;
            }
            let command = key.modifiers.control
                || key.modifiers.platform
                || key.modifiers.alt
                || key.modifiers.function;
            if !command
                && (key.key_char.is_some() || key.key.chars().count() == 1 || key.key == "space")
                && !matches!(
                    key.key.as_str(),
                    "enter" | "tab" | "escape" | "backspace" | "delete"
                )
            {
                return;
            }
        }
        if event.is_held && matches!(key.key.as_str(), "escape" | "enter") {
            cx.stop_propagation();
            return;
        }
        if let Some(action) = settings.key(event, area) {
            if action != Action::Apply {
                self.held_close_key = Some(key.key.clone());
            }
            self.settings_action(action, window, cx);
        } else {
            self.repaint_settings(window, cx);
        }
        cx.stop_propagation();
    }

    /// Ignore the scrim outside the dialog and route visible cells to the grid window.
    pub(super) fn click_settings(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_writing {
            return;
        }
        let area = self.settings_area();
        if !area.pixels(self.metrics).contains(&point) {
            return;
        }
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        #[cfg(windows)]
        let composing = self
            .settings
            .as_ref()
            .is_some_and(|s| !s.composition.is_empty());
        let action = self
            .settings
            .as_mut()
            .and_then(|s| s.click(point, area, self.metrics, &self.spacing));
        // Moving directly between inputs keeps the IME attached, but its old candidate must be cancelled.
        #[cfg(windows)]
        if composing
            && self
                .settings
                .as_ref()
                .is_some_and(|s| s.composition.is_empty())
        {
            self.sync_native_ime(window, cx, true);
        }
        if let Some(action) = action {
            self.settings_action(action, window, cx);
        } else {
            self.repaint_settings(window, cx);
        }
    }

    /// Scroll the hovered settings region using logical rows and the shared frame clock.
    pub(super) fn wheel_settings(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let metrics = self.metrics;
        let rows = -f32::from(event.delta.pixel_delta(px(metrics.lh)).y) / metrics.lh;
        self.cursor
            .update(cx, |cursor, cx| cursor.activity(window, cx));
        self.track_settings_scroll(cx, |settings, area, now| {
            settings.wheel(event.position, rows, area, metrics, now)
        });
        self.repaint_settings(window, cx);
    }

    /// Rebuild the foreground panels without scrolling the covered project.
    pub(super) fn refresh_settings(&self, cx: &mut App) {
        if let Some(settings) = &self.settings {
            settings.refresh(self.settings_area(), &self.spacing, cx);
        }
    }

    /// Run fractional scrolling and follow the active row after navigation or resize.
    pub(super) fn advance_settings(&mut self, cx: &mut Context<Self>) {
        self.track_settings_scroll(cx, Settings::advance);
    }

    /// Rebase the cursor's entire animation by the movement of its panel, including a
    /// floating selector moving with its anchor while either viewport is scrolling.
    fn track_settings_scroll(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut Settings, Rect, f64),
    ) {
        let area = self.settings_area();
        let Some(settings) = &mut self.settings else {
            return;
        };
        let before = settings.cursor_viewport(area).origin_y();
        update(settings, area, self.scroll_epoch.elapsed().as_secs_f64());
        let settings = self.settings.as_ref().unwrap();
        if self.last_cursor_space == Some(CursorSpace::Settings(settings.cursor_region())) {
            let viewport = settings.cursor_viewport(area);
            self.follow_cursor_scroll(viewport.scroll, before - viewport.origin_y(), None, cx);
        }
    }

    /// Update IME ownership and paint only settings after a local edit or gesture.
    pub(super) fn repaint_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_text_input(window, cx);
        self.advance_settings(cx);
        self.refresh_settings(cx);
        self.update_cursor(window, cx);
        self.schedule_scroll(window, cx);
        cx.notify();
    }
}
