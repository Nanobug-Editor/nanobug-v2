//! Route an editor's Vim menu across native keyboard, pointer and retained rendering.

use super::{
    Workspace,
    layout::{FOOTER_ROWS, Focus},
};
use crate::grid::{
    render::Rect,
    windows::{editor::EditorWindow, vim_menu::VimMenu},
};
use gpui::{
    ClipboardItem, Context, KeyDownEvent, MouseDownEvent, Pixels, Point, ScrollEvent, Window, px,
};
use nanobug::{animation::scroll::ScrollInput, i18n};

impl Workspace {
    /// The grid viewport below the caption and above the status line.
    fn vim_menu_viewport(&self) -> Rect {
        let y = self.layout.editor.y.min(self.layout.visible_rows);
        let bottom = self
            .layout
            .visible_rows
            .min(self.layout.rows.saturating_sub(FOOTER_ROWS));
        Rect {
            x: 0,
            y,
            w: self.layout.visible_cols,
            h: bottom.saturating_sub(y),
        }
    }

    /// Fit the current Vim menu inside the visible grid.
    pub(super) fn vim_menu_area(&self) -> Option<Rect> {
        self.vim_menu
            .as_ref()?
            .area(self.vim_menu_viewport(), &self.spacing)
    }

    /// Present the configured menu at `path` next to the original editor caret.
    pub(super) fn popup_vim_menu(
        &mut self,
        path: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let (x, y, _, _, _) = self.caret();
        let anchor = (x.max(0.) as usize, y.max(0.) as usize + 1);
        if !self.show_vim_menu(path, anchor, window, cx) {
            self.set_status(i18n::text("vim_menu.no_entries").into());
        }
    }

    /// Resolve one menu level using the focused editor and show it at `anchor`.
    fn show_vim_menu(
        &mut self,
        path: Vec<String>,
        anchor: (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let viewport = self.vim_menu_viewport();
        if viewport.w < 10 || viewport.h < 3 {
            return false;
        }
        let Some(project) = self
            .projects
            .active_project_mut()
            .filter(|project| project.focus == Focus::Editor)
        else {
            return false;
        };
        let editor = &mut project.editor;
        let entries = editor.vim().menu_entries(editor.register(), &path);
        if entries.is_empty() {
            return false;
        }
        let opening = editor.vim().doc.opening;
        editor.stop_drag();
        self.prefix = None;
        self.dismiss_completion(cx);
        self.dismiss_definition_hover(cx);
        self.dismiss_sequence_hint(cx);
        self.vim_menu = Some(VimMenu::new(
            opening,
            anchor,
            path,
            entries,
            self.metrics,
            cx,
        ));
        self.sync(window, cx);
        true
    }

    /// Route a fresh right click to PopUp without changing the editor's selection.
    pub(super) fn right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.is_some() {
            cx.stop_propagation();
            return;
        }
        if self.vim_menu.is_some() {
            self.close_vim_menu(window, cx);
            cx.stop_propagation();
            return;
        }
        let (x, y) = self.metrics.cell(event.position);
        if !self.modal_open()
            && EditorWindow::body(self.active_editor_area()).contains(x, y)
            && self
                .projects
                .active_project()
                .is_some_and(|project| project.focus == Focus::Editor)
        {
            self.activate_input_mappings();
            if self.show_vim_menu(vec!["PopUp".into()], (x, y), window, cx) {
                cx.stop_propagation();
                return;
            }
        }
        if self.has_canvas_button_callbacks() {
            self.plugin_canvas_aux_down(event, window, cx);
        }
    }

    /// Close the current Vim menu and restore its original editor's native input.
    fn close_vim_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.vim_menu = None;
        self.sync(window, cx);
    }

    /// Enter the selected submenu or dispatch its action against the menu's original editor.
    fn activate_vim_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.vim_menu else { return };
        let Some(entry) = menu.selected().cloned() else {
            return;
        };
        if !self.projects.active_project().is_some_and(|project| {
            project.focus == Focus::Editor && project.editor.vim().doc.opening == menu.opening
        }) {
            self.close_vim_menu(window, cx);
            return;
        }
        if entry.submenu {
            self.show_vim_menu(entry.path, menu.anchor, window, cx);
            return;
        }
        self.vim_menu = None;
        let area = self.active_editor_area();
        let editor = &mut self.projects.active_mut().editor;
        let yank = editor.register().yank_revision();
        let edit = editor.activate_vim_menu(&entry.path, area, &self.spacing);
        if editor.register().yank_revision() != yank {
            cx.write_to_clipboard(ClipboardItem::new_string(editor.register().text.clone()));
        }
        self.apply_edit(edit, window, cx);
        self.sync(window, cx);
        self.track_mapping_timeout(window, cx);
        self.sync_sequence_hint_delay(cx);
    }

    /// Translate `event` into popup selection, mnemonic activation, or dismissal.
    pub(super) fn vim_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = self
            .vim_menu_area()
            .map_or(1, |area| area.h.saturating_sub(2)) as isize;
        let menu = self.vim_menu.as_mut().expect("visible Vim menu");
        let key = &event.keystroke;
        let mods = key.modifiers;
        let raw = key.key.as_str();
        if (!mods.modified() && raw == "escape") || mods.control && !mods.alt && raw == "c" {
            self.held_close_key = Some(raw.into());
            self.close_vim_menu(window, cx);
            return;
        }
        if !mods.modified() {
            let delta = match raw {
                "up" | "k" => -1,
                "down" | "j" => 1,
                "pageup" => -page,
                "pagedown" => page,
                "home" => isize::MIN,
                "end" => isize::MAX,
                _ => 0,
            };
            if delta != 0 {
                menu.move_selection(delta);
                self.scroll_frame(
                    [
                        ScrollInput::Advance,
                        ScrollInput::Reveal,
                        ScrollInput::Advance,
                    ],
                    window,
                    cx,
                );
                return;
            }
            if matches!(raw, "left" | "h" | "backspace") {
                let mut path = menu.path.clone();
                let anchor = menu.anchor;
                if path.pop().is_none() {
                    self.close_vim_menu(window, cx);
                } else {
                    self.show_vim_menu(path, anchor, window, cx);
                }
                return;
            }
            if matches!(raw, "enter" | "right" | "l") {
                if !event.is_held {
                    self.held_close_key = Some(raw.into());
                    self.activate_vim_menu(window, cx);
                }
                return;
            }
        }
        if !event.is_held
            && !mods.control
            && !mods.platform
            && !mods.function
            && let [letter] = raw.as_bytes()
            && letter.is_ascii_alphabetic()
            && menu.select_mnemonic(char::from(*letter))
        {
            self.held_close_key = Some(raw.into());
            self.activate_vim_menu(window, cx);
        }
    }

    /// Select the menu item pointed at by `point` in the currently painted, scrolling rows.
    fn point_at_vim_menu(&mut self, point: Point<Pixels>) -> bool {
        let area = self.vim_menu_area();
        self.vim_menu
            .as_mut()
            .zip(area)
            .is_some_and(|(menu, area)| menu.select_point(point, area, self.metrics))
    }

    /// Hover over an item under `point`, keeping other editor pointer handling suspended.
    pub(super) fn hover_vim_menu(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) {
        if self.point_at_vim_menu(point) {
            self.refresh_vim_menu(cx);
            cx.notify();
        }
    }

    /// Activate the enabled row at `point` or dismiss the menu on an outside click.
    pub(super) fn click_vim_menu(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.point_at_vim_menu(point) {
            self.activate_vim_menu(window, cx);
        } else if self
            .vim_menu_area()
            .is_none_or(|area| !area.pixels(self.metrics).contains(&point))
        {
            self.close_vim_menu(window, cx);
        }
    }

    /// Scroll the menu under `event`, consuming the wheel while the popup owns input.
    pub(super) fn wheel_vim_menu(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.vim_menu_area();
        if !area.is_some_and(|area| area.pixels(self.metrics).contains(&event.position)) {
            return;
        }
        let delta = f32::from(event.delta.pixel_delta(px(self.metrics.lh)).y);
        let rows = self
            .vim_menu
            .as_mut()
            .expect("visible Vim menu")
            .wheel_rows(-delta / self.metrics.lh);
        self.scroll_frame(
            [
                ScrollInput::Advance,
                ScrollInput::Wheel(rows),
                ScrollInput::Advance,
            ],
            window,
            cx,
        );
    }

    /// Advance menu scroll from the shared frame loop with the row `input`.
    pub(super) fn advance_vim_menu(&mut self, input: ScrollInput) {
        let area = self.vim_menu_area();
        if let Some((menu, area)) = self.vim_menu.as_mut().zip(area) {
            menu.advance(input, area, self.scroll_epoch.elapsed().as_secs_f64());
        }
    }

    /// Redraw the retained menu frames for the current position and geometry through `cx`.
    pub(super) fn refresh_vim_menu(&mut self, cx: &mut Context<Self>) {
        if let Some((menu, area)) = self.vim_menu.as_ref().zip(self.vim_menu_area()) {
            menu.refresh(area, &self.spacing, cx);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::native_test;
    use anyhow::{Context as _, Result, ensure};
    use gpui::{AppContext, EntityInputHandler, Keystroke, MouseButton, size};
    use nanobug::{plugins::session::Options, vim::Mode};
    use std::{fs, time::Duration};

    /// Send one native `key` to the workspace.
    fn press(
        workspace: &mut Workspace,
        key: &'static str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Result<()> {
        workspace.key_down(
            &KeyDownEvent {
                keystroke: Keystroke::parse(key)?,
                is_held: false,
                prefer_character_input: false,
            },
            window,
            cx,
        );
        Ok(())
    }

    /// Enter an Ex `command` using the same keyboard and text input paths as the app.
    fn ex(
        workspace: &mut Workspace,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Result<()> {
        press(workspace, ":", window, cx)?;
        workspace.replace_text_in_range(None, command, window, cx);
        press(workspace, "enter", window, cx)
    }

    /// Select a menu entry with the mouse at its actual rendered row on the current level.
    fn click(
        workspace: &mut Workspace,
        path: &[String],
        label: &str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Result<()> {
        let area = workspace.vim_menu_area().context("Vim menu not visible")?;
        let editor = &workspace.projects.active().editor;
        let row = editor
            .vim()
            .menu_entries(editor.register(), path)
            .iter()
            .position(|entry| entry.label == label)
            .context("Vim menu row missing")?;
        ensure!(
            row + 2 < area.h,
            "Fixture row lies outside the menu viewport"
        );
        workspace.mouse_down(
            &MouseDownEvent {
                button: MouseButton::Left,
                position: workspace
                    .metrics
                    .at(area.x as f32 + 2.5, (area.y + row) as f32 + 1.5),
                click_count: 1,
                ..Default::default()
            },
            window,
            cx,
        );
        Ok(())
    }

    /// Exercise actual right-click activation in Select mode, then open a nested menu via :popup.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_vim_menu_click_preserves_or_exits_select_with_gv() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        fs::write(project.join("README.md"), "abcdef\n")?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                app.update_window(handle.into(), |_, window, _| {
                    window.resize(size(px(920.), px(580.)))
                })?;
                app.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    for command in [
                        "vnoremenu PopUp.&Keep <Esc>",
                        "vnoremenu PopUp.&Exit <Esc>gV",
                        "nnoremenu Tools.Edit.Step l",
                        "tmenu PopUp.Exit Finish the selection",
                    ] {
                        ex(workspace, command, window, cx)?;
                    }
                    for key in ["g", "h", "right", "right"] {
                        press(workspace, key, window, cx)?;
                    }
                    let vim = workspace.projects.active().editor.vim();
                    let selected = vim.selection().context("Select region missing")?;
                    ensure!(vim.accepts_text());
                    let popup = |workspace: &mut Workspace,
                                 window: &mut Window,
                                 cx: &mut Context<Workspace>| {
                        let editor = workspace.active_editor_area();
                        workspace.right_mouse_down(
                            &MouseDownEvent {
                                button: MouseButton::Right,
                                position: workspace
                                    .metrics
                                    .at(editor.x as f32 + 4.5, editor.y as f32 + 2.5),
                                click_count: 1,
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    };
                    let path = vec!["PopUp".into()];
                    popup(workspace, window, cx);
                    ensure!(!workspace.accepts_native_text());
                    let area = workspace.vim_menu_area().context("Vim menu not visible")?;
                    ensure!(
                        area.h == 5,
                        "The tips need their own row below both entries"
                    );
                    workspace.hover_vim_menu(
                        workspace
                            .metrics
                            .at(area.x as f32 + 2.5, area.y as f32 + 2.5),
                        cx,
                    );
                    ensure!(
                        workspace
                            .vim_menu
                            .as_ref()
                            .and_then(|menu| menu.selected())
                            .and_then(|entry| entry.tooltip.as_deref())
                            == Some("Finish the selection")
                    );
                    click(workspace, &path, "Keep", window, cx)?;
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.accepts_text() && vim.selection().as_ref() == Some(&selected));
                    popup(workspace, window, cx);
                    click(workspace, &path, "Exit", window, cx)?;
                    let vim = workspace.projects.active().editor.vim();
                    ensure!(vim.mode == Mode::Normal && vim.selection().is_none());
                    for key in ["g", "v"] {
                        press(workspace, key, window, cx)?;
                    }
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .selection()
                            .as_ref()
                            == Some(&selected)
                    );
                    ex(workspace, "popup PopUp", window, cx)?;
                    ensure!(workspace.vim_menu.is_some());
                    press(workspace, "escape", window, cx)?;
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .selection()
                            .as_ref()
                            == Some(&selected)
                    );
                    press(workspace, "escape", window, cx)?;
                    press(workspace, "0", window, cx)?;
                    ex(workspace, "popup Tools", window, cx)?;
                    click(workspace, &["Tools".into()], "Edit", window, cx)?;
                    click(
                        workspace,
                        &["Tools".into(), "Edit".into()],
                        "Step",
                        window,
                        cx,
                    )?;
                    ensure!(workspace.vim_menu.is_none());
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 1);
                    for index in 0..24 {
                        ex(
                            workspace,
                            &format!("nnoremenu Long.Row{index:02} l"),
                            window,
                            cx,
                        )?;
                    }
                    ex(workspace, "popup Long", window, cx)?;
                    press(workspace, "end", window, cx)?;
                    let menu = workspace
                        .vim_menu
                        .as_ref()
                        .context("Long menu not visible")?;
                    ensure!(menu.selected().is_some_and(|entry| entry.label == "Row23"));
                    ensure!(menu.scrolling());
                    press(workspace, "down", window, cx)?;
                    ensure!(
                        workspace
                            .vim_menu
                            .as_ref()
                            .and_then(VimMenu::selected)
                            .is_some_and(|entry| entry.label == "Row23")
                    );
                    press(workspace, "home", window, cx)?;
                    ensure!(
                        workspace
                            .vim_menu
                            .as_ref()
                            .and_then(VimMenu::selected)
                            .is_some_and(|entry| entry.label == "Row00")
                    );
                    press(workspace, "escape", window, cx)?;
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 1);
                    Ok(())
                })?
            },
        )
    }
}
