//! Verify custom binding rows in the delayed workspace hint.

use super::*;
use gpui::EntityInputHandler;
use nanobug::{
    i18n::{self, Locale},
    vim::Mode,
};

/// Show applicable right-hand sides across key and committed-text input modes.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_custom_mapping_hint_lists_mode_bindings_after_delay() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "cat dog\n")?;
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
                for definition in [
                    "nnoremap z<C-Left> :echo 'named key'<CR>",
                    "nnoremap zx :echo 'fast'<CR>",
                    "nnoremap zt :echo 'short'<CR>",
                    "nnoremap ztq :echo 'nested'<CR>",
                    "nnoremap zm :echo 'global'<CR>",
                    "nnoremap <buffer> zm :echo 'local'<CR>",
                    "inoremap zv NEW",
                    "cnoremap ze echo 'command'<CR>",
                    "onoremap zo iw",
                ] {
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, definition, window, cx);
                    press(workspace, "enter", window, cx)?;
                }

                press_hidden(workspace, "z", window, cx)?;
                press(workspace, "x", window, cx)?;
                ensure!(workspace.projects.active().editor.vim().message == "fast");
                ensure!(workspace.sequence_hint_area().is_none());
                Ok(())
            })??;
            app.background_executor().timer(HINT_SETTLE).await;

            handle.update(app, |workspace, window, cx| -> Result<()> {
                ensure!(workspace.sequence_hint_area().is_none(), "stale zx hint");
                press_hidden(workspace, "z", window, cx)
            })??;
            app.background_executor().timer(HINT_SETTLE).await;
            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace.visible_sequence_hint().context("custom z hint")?;
                ensure!(
                    hint.rows()
                        == [
                            ("<C-Left>", ":echo 'named key'<CR>", false),
                            ("m", ":echo 'local'<CR>", false),
                            ("t", ":echo 'short'<CR>", false),
                            ("tq", ":echo 'nested'<CR>", false),
                            ("x", ":echo 'fast'<CR>", false),
                        ]
                );
                let original_locale = i18n::locale();
                i18n::set_locale(Locale::English);
                let english = hint.title();
                i18n::set_locale(Locale::SimplifiedChinese);
                let chinese = hint.title();
                i18n::set_locale(original_locale);
                ensure!(english == "z[+mapping]" && chinese == "z[+映射]");

                press(workspace, "t", window, cx)?;
                let hint = workspace
                    .visible_sequence_hint()
                    .context("custom zt hint")?;
                ensure!(
                    hint.rows()
                        == [
                            ("q", ":echo 'nested'<CR>", false),
                            (
                                i18n::text("vim_mapping.timeout"),
                                ":echo 'short'<CR>",
                                false
                            ),
                        ]
                );
                press(workspace, "q", window, cx)?;
                ensure!(workspace.projects.active().editor.vim().message == "nested");
                ensure!(workspace.sequence_hint_area().is_none());

                press(workspace, "i", window, cx)?;
                workspace.replace_text_in_range(None, "z", window, cx);
                ensure!(workspace.sequence_hint_area().is_none());
                Ok(())
            })??;
            app.background_executor().timer(HINT_SETTLE).await;

            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace.visible_sequence_hint().context("Insert z hint")?;
                ensure!(hint.rows() == [("v", "NEW", false)]);
                workspace.replace_text_in_range(None, "v", window, cx);
                ensure!(workspace.projects.active().editor.vim().doc.text == "NEWcat dog\n");
                ensure!(workspace.sequence_hint_area().is_none());
                press(workspace, "escape", window, cx)?;

                press(workspace, ":", window, cx)?;
                workspace.replace_text_in_range(None, "z", window, cx);
                ensure!(workspace.sequence_hint_area().is_none());
                Ok(())
            })??;
            app.background_executor().timer(HINT_SETTLE).await;

            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace
                    .visible_sequence_hint()
                    .context("Command-line z hint")?;
                ensure!(hint.rows() == [("e", "echo 'command'<CR>", false)]);
                workspace.replace_text_in_range(None, "e", window, cx);
                let vim = workspace.projects.active().editor.vim();
                ensure!(vim.message == "command" && vim.mode == Mode::Normal);
                ensure!(workspace.sequence_hint_area().is_none());

                press(workspace, "d", window, cx)?;
                press_hidden(workspace, "z", window, cx)
            })??;
            app.background_executor().timer(HINT_SETTLE).await;

            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace
                    .visible_sequence_hint()
                    .context("Operator-pending z hint")?;
                ensure!(hint.rows() == [("o", "iw", false)]);
                press(workspace, "o", window, cx)?;
                ensure!(workspace.projects.active().editor.vim().doc.text == " dog\n");
                ensure!(workspace.sequence_hint_area().is_none());
                Ok(())
            })?
        },
    )
}

/// Page through a shared Leader prefix and then finish the custom mapping.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn native_custom_leader_hint_pages_without_releasing_the_prefix() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "fixture\n")?;
    let options = Options::take(&mut vec!["--no-plugins".into()], temp.path())?;
    native_test::run_async(
        project,
        options,
        |workspace| workspace.projects.active_mut().focus_pane(Focus::Editor),
        async move |handle, app| {
            let _keep_temp = temp;
            handle.update(app, |workspace, window, cx| {
                let mut appearance = workspace.appearance;
                appearance.base_size = 24.;
                appearance.zoom = 1.5;
                workspace.apply_appearance(appearance, window, cx);
                window.resize(size(px(800.), px(480.)));
            })?;
            app.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            handle.update(app, |workspace, window, cx| -> Result<()> {
                workspace.sync(window, cx);
                for (first, last) in
                    ('a'..='z').flat_map(|first| ['a', 'b'].map(|last| (first, last)))
                {
                    let definition =
                        format!("nnoremap <Leader>{first}{last} :echo '{first}{last}'<CR>");
                    press(workspace, ":", window, cx)?;
                    workspace.replace_text_in_range(None, &definition, window, cx);
                    press(workspace, "enter", window, cx)?;
                }
                press_hidden(workspace, "space", window, cx)
            })??;
            app.background_executor().timer(HINT_SETTLE).await;

            handle.update(app, |workspace, window, cx| -> Result<()> {
                let hint = workspace
                    .visible_sequence_hint()
                    .context("Leader mapping hint")?;
                ensure!(hint.rows().last() == Some(&("zb", ":echo 'zb'<CR>", false)));
                let pages = hint
                    .len()
                    .div_ceil(workspace.sequence_hint_layout(&hint).page_size);
                ensure!(pages > 1, "the fixture should require pagination");
                let original_prefix = workspace
                    .projects
                    .active()
                    .editor
                    .vim()
                    .mapping_timeout_token();
                ensure!(original_prefix.is_some());
                for page in 1..pages {
                    press(workspace, "pagedown", window, cx)?;
                    ensure!(workspace.hint_page == page);
                    ensure!(
                        workspace
                            .projects
                            .active()
                            .editor
                            .vim()
                            .mapping_timeout_token()
                            == original_prefix
                    );
                }
                press(workspace, "z", window, cx)?;
                ensure!(workspace.hint_page == 0);
                let hint = workspace
                    .visible_sequence_hint()
                    .context("Leader z mapping hint")?;
                ensure!(
                    hint.rows()
                        == [
                            ("a", ":echo 'za'<CR>", false),
                            ("b", ":echo 'zb'<CR>", false)
                        ]
                );
                press(workspace, "b", window, cx)?;
                ensure!(workspace.projects.active().editor.vim().message == "zb");
                ensure!(workspace.sequence_hint_area().is_none());
                Ok(())
            })?
        },
    )
}
