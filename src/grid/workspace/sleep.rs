//! Pause modal keys without blocking native rendering and background events.

use super::{Workspace, layout::Focus};
use crate::{
    grid::{
        render::Rect,
        windows::editor::{EditResult, EditorWindow},
    },
    ui::text::TextSpacing,
};
use gpui::{ClipboardItem, Context, KeyDownEvent, Modifiers, Task, Window};
use nanobug::{animation::scroll::ScrollInput, vim::Action};
use std::time::Duration;

/// Work released after an Ex sleep still belongs to the active source view.
type ExContinuation = Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// Which editor suspended playback and receives the timer result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// Active project editor.
    Editor,
    /// Focused file-picker editor.
    Preview,
    /// Temporary command history editor.
    History,
}

/// One interruptible Vim sleep and its pending native keystrokes.
pub(super) struct Sleep {
    /// Dropping the task cancels the wake-up callback.
    _task: Task<()>,
    /// Source editor and unique opening identity when the timer started.
    source: (Source, u64),
    /// Ordinary keys dispatched after the timeout if it is not interrupted.
    queued: Vec<KeyDownEvent>,
    /// Whether the Ex bang hides the cursor during this pause.
    hide_caret: bool,
    /// Sourced Ex statements that resume when the delay completes.
    resume_ex: Option<ExContinuation>,
}

impl Workspace {
    /// Pause modal input in `window` for `seconds`, using `cx` to queue a wake-up.
    pub(super) fn sleep_editor(
        &mut self,
        seconds: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sleep_editor_for(Duration::from_secs(seconds), false, window, cx);
    }

    /// Pause native input for `duration`, with the cursor hidden for an Ex bang.
    pub(super) fn sleep_editor_for(
        &mut self,
        duration: Duration,
        hide_caret: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_editor_sleep(duration, hide_caret, None, window, cx);
    }

    /// Pause a sourced script, resuming only after the originating view's timer finishes.
    pub(super) fn sleep_with_ex_continuation(
        &mut self,
        duration: Duration,
        hide_caret: bool,
        resume: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_editor_sleep(duration, hide_caret, Some(Box::new(resume)), window, cx);
    }

    /// Whether a nested sourced script has deferred its remaining statements.
    pub(super) fn ex_sleep_is_pending(&self) -> bool {
        self.editor_sleep
            .as_ref()
            .is_some_and(|sleep| sleep.resume_ex.is_some())
    }

    /// Release a caller's remaining statements after the nested source finishes waiting.
    pub(super) fn then_ex_sleep(
        &mut self,
        resume: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) -> bool {
        self.append_ex_sleep(None, Box::new(resume)).is_ok()
    }

    /// Chain work to a source timer, retaining its original view across successive pauses.
    fn append_ex_sleep(
        &mut self,
        source: Option<(Source, u64)>,
        resume: ExContinuation,
    ) -> Result<(), ExContinuation> {
        let Some(sleep) = &mut self.editor_sleep else {
            return Err(resume);
        };
        if source.is_some_and(|source| source != sleep.source) {
            return Err(resume);
        }
        let Some(previous) = sleep.resume_ex.take() else {
            return Err(resume);
        };
        let source = sleep.source;
        sleep.resume_ex = Some(Box::new(move |workspace, window, cx| {
            previous(workspace, window, cx);
            if let Err(resume) = workspace.append_ex_sleep(Some(source), resume)
                && workspace.editor_sleep.is_none()
            {
                resume(workspace, window, cx);
            }
        }));
        Ok(())
    }

    /// Create the shared interruptible timer for typed input and Ex source continuations.
    fn start_editor_sleep(
        &mut self,
        duration: Duration,
        hide_caret: bool,
        resume_ex: Option<ExContinuation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((source, editor)) = self.active_vim_input() else {
            return;
        };
        let source = (source, editor.vim().doc.opening);
        let timer = cx.background_executor().timer(duration);
        let task = cx.spawn_in(window, async move |workspace, cx| {
            timer.await;
            let _ = cx.update(|window, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.finish_editor_sleep(false, window, cx);
                });
            });
        });
        self.editor_sleep = Some(Sleep {
            _task: task,
            source,
            queued: Vec::new(),
            hide_caret,
            resume_ex,
        });
        if hide_caret {
            self.update_cursor(window, cx);
        }
    }

    /// Report whether an active Ex sleep currently suppresses the editor cursor.
    pub(super) fn sleep_hides_caret(&self) -> bool {
        self.editor_sleep
            .as_ref()
            .is_some_and(|sleep| sleep.hide_caret)
    }

    /// Defer `event` during a sleep, or cancel the delay and typeahead on Ctrl+C.
    pub(super) fn pause_editor_input(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(sleep) = &mut self.editor_sleep else {
            return false;
        };
        let key = &event.keystroke;
        let escape_source = sleep.resume_ex.is_some()
            && key.key == "escape"
            && key.modifiers == Modifiers::default();
        if escape_source
            || key.key == "c"
                && key.modifiers
                    == (Modifiers {
                        control: true,
                        ..Modifiers::default()
                    })
        {
            self.finish_editor_sleep(true, window, cx);
        } else if sleep.queued.len() < 10_000 {
            sleep.queued.push(event.clone());
        }
        true
    }

    /// Return the source and editor for the currently active Vim keyboard layer.
    pub(super) fn active_vim_input(&self) -> Option<(Source, &EditorWindow)> {
        if let Some(history) = &self.command_window {
            Some((Source::History, &history.editor))
        } else if self.help.is_open() || self.small_modal_open() || self.plugins.owns_input() {
            None
        } else if let Some(preview) = &self.file_picker {
            Some((Source::Preview, preview.focused_editor()?))
        } else {
            let project = self.projects.active_project()?;
            (project.focus == Focus::Editor).then_some((Source::Editor, &project.editor))
        }
    }

    /// Resume the source buffer and release its pending keys, or cancel both when interrupted.
    fn finish_editor_sleep(
        &mut self,
        interrupted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sleep) = self.editor_sleep.take() else {
            return;
        };
        if !self
            .active_vim_input()
            .is_some_and(|(source, editor)| (source, editor.vim().doc.opening) == sleep.source)
        {
            if sleep.hide_caret {
                self.update_cursor(window, cx);
            }
            return;
        }
        self.finish_deferred_vim_input(sleep.source.0, window, cx, |editor, area, spacing| {
            if interrupted {
                editor.edit(|vim, _| {
                    vim.interrupt_sleep();
                    Action::None
                })
            } else {
                editor.resume_sleep(area, spacing)
            }
        });
        if !interrupted {
            if let Some(resume) = sleep.resume_ex {
                resume(self, window, cx);
            }
            for key in sleep.queued {
                self.key_down(&key, window, cx);
            }
        }
    }

    /// Apply a deferred edit to the validated editor `source` and dispatch its effects.
    pub(super) fn finish_deferred_vim_input(
        &mut self,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut EditorWindow, Rect, &TextSpacing) -> EditResult,
    ) {
        let (editor, area) = match source {
            Source::Editor => {
                let area = self.active_editor_area();
                (&mut self.projects.active_mut().editor, area)
            }
            Source::Preview => {
                let area = self.file_picker_layout().preview_editor_area();
                let editor = self
                    .file_picker
                    .as_mut()
                    .and_then(|picker| picker.focused_editor_mut())
                    .expect("validated preview");
                (editor, area)
            }
            Source::History => {
                let area = self.command_window_area().expect("validated history");
                (
                    &mut self
                        .command_window
                        .as_mut()
                        .expect("validated history")
                        .editor,
                    area,
                )
            }
        };
        let before = editor.register().yank_revision();
        let edit = apply(editor, area, &self.spacing);
        if editor.register().yank_revision() != before {
            cx.write_to_clipboard(ClipboardItem::new_string(editor.register().text.clone()));
        }
        match source {
            Source::Editor => {
                self.apply_edit(edit, window, cx);
                self.sync(window, cx);
            }
            Source::Preview => {
                self.finish_focused_file_action(
                    edit.action,
                    "@",
                    [ScrollInput::Reveal; 2],
                    window,
                    cx,
                );
            }
            Source::History => {
                self.finish_command_window_action(
                    edit.action,
                    [ScrollInput::Reveal; 2],
                    window,
                    cx,
                );
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::grid::workspace::{layout::Focus, native_test};
    use anyhow::{Result, ensure};
    use gpui::Keystroke;
    use nanobug::plugins::session::Options;
    use std::{fs, time::Instant};

    /// Resume macro strokes before typed keys after a native timer, and cancel both on interrupt.
    #[test]
    #[ignore = "opens a native Windows workspace"]
    fn native_gs_defers_and_interrupts_keystrokes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        let source = "abcd\n";
        fs::write(project.join("README.md"), source)?;
        let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
        native_test::run_async(
            project,
            options,
            |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
            async move |handle, app| {
                let _keep_temp = temp;
                let send = |workspace: &mut Workspace,
                            window: &mut Window,
                            cx: &mut Context<Workspace>,
                            keys: &[&'static str]|
                 -> Result<()> {
                    for key in keys {
                        workspace.key_down(
                            &KeyDownEvent {
                                keystroke: Keystroke::parse(key)?,
                                is_held: false,
                                prefer_character_input: false,
                            },
                            window,
                            cx,
                        );
                    }
                    Ok(())
                };
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    workspace.sync(window, cx);
                    Ok(())
                })??;
                for (keys, before, after) in [
                    (&["q", "a", "g", "s", "l", "q"][..], 0, 1),
                    (&["shift-2", "a", "l"][..], 1, 3),
                ] {
                    handle.update(app, |workspace, window, cx| -> Result<()> {
                        send(workspace, window, cx, keys)?;
                        ensure!(workspace.editor_sleep.is_some());
                        ensure!(workspace.projects.active().editor.vim().doc.cursor == before);
                        Ok(())
                    })??;
                    let deadline = Instant::now() + Duration::from_secs(3);
                    loop {
                        let done = handle.update(app, |workspace, _, _| {
                            workspace.editor_sleep.is_none()
                                && workspace.projects.active().editor.vim().doc.cursor == after
                        })?;
                        if done {
                            break;
                        }
                        ensure!(
                            Instant::now() < deadline,
                            "Vim gs did not resume playback before buffered input"
                        );
                        app.background_executor()
                            .timer(Duration::from_millis(20))
                            .await;
                    }
                }
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    send(
                        workspace,
                        window,
                        cx,
                        &["2", "shift-2", "a", "x", "ctrl-c", "h"],
                    )?;
                    ensure!(workspace.editor_sleep.is_none());
                    ensure!(workspace.projects.active().editor.vim().doc.text == source);
                    ensure!(workspace.projects.active().editor.vim().doc.cursor == 2);
                    Ok(())
                })?
            },
        )
    }
}
