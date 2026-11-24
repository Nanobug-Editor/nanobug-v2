//! Exercise the native prompt and close confirmation with a temporary credential only.

use super::{Input, Prompt, Workspace};
use crate::grid::workspace::{editor_close::CloseTarget, native_test};
use anyhow::{Context as _, Result, ensure};
use gpui::{Context, EntityInputHandler, KeyDownEvent, Keystroke, Window, WindowHandle};
use nanobug::plugins::{
    package::InstallStore,
    session::{Options, Session},
};
use nanobug_plugin_sdk::Permission;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[test]
#[ignore = "requires offscreen Windows GPUI and temporary Windows Credential Manager entries"]
fn masked_credential_close_handles_continue_failure_discard_and_apply() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    let profile = fixture.path().join("profile");
    let archive = fixture.path().join("fixture.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive)?);
    zip.start_file(
        "nanobug-plugin.toml",
        zip::write::SimpleFileOptions::default(),
    )?;
    zip.write_all(
        br#"
name='Credential Modal Fixture'
id='credential-modal-fixture'
version='1.0.0'
permissions=['credentials']
[engines]
nanobug='^0.1.0'
[settings.login]
title='Login'
secret=true
"#,
    )?;
    zip.finish()?;
    let store = InstallStore::new(profile.clone());
    let prepared = store.prepare(&archive)?;
    let identity = prepared.release.manifest.identity();
    let namespace = identity.namespace();
    store.install(prepared, &BTreeSet::from([Permission::Credentials]))?;
    let options = || {
        Options::take(
            &mut vec!["--plugin-profile".into(), profile.as_os_str().into()],
            fixture.path(),
        )
    };
    let host = Session::start(options()?).0;
    let expires = Instant::now() + Duration::from_secs(5);
    while host.view().installed.is_empty() {
        ensure!(
            Instant::now() < expires,
            "Temporary plugin could not be loaded"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
    let index = profile.join("credentials.json");
    fs::write(&index, "malformed fixture")?;
    native_test::run_async(
        project,
        options()?,
        move |workspace| {
            workspace.plugins.options = None;
            workspace.plugins.session = Some(host);
        },
        async move |handle, cx| {
            let marker = "只用临时凭据";
            cx.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    open_prompt(workspace, &namespace);
                    ensure!(workspace.accepts_native_text());
                    workspace.replace_and_mark_text_in_range(None, "暂", Some(1..1), window, cx);
                    ensure!(workspace.marked_text_range(window, cx) == Some(0..1));
                    workspace.replace_text_in_range(None, marker, window, cx);
                    ensure!(workspace.marked_text_range(window, cx).is_none());
                    let (_, masked) = workspace.plugin_manager_view().plugin_prompt_text(80);
                    ensure!(!masked.contains(marker) && masked.chars().all(|ch| ch == '•'));
                    ensure!(workspace.prepare_plugin_close(CloseTarget::Tab, window, cx));
                    ensure!(!workspace.accepts_native_text());
                    ensure!(workspace.plugins.credential_close == Some(CloseTarget::Tab));
                    ensure!(workspace.plugins.rows().len() == 3);
                    workspace.plugins.manager.selection = 1;
                    key(workspace, "enter", window, cx)?;
                    ensure!(workspace.accepts_native_text());
                    ensure!(workspace.plugins.credential_close.is_none());
                    key(workspace, "enter", window, cx)?;
                    ensure!(!workspace.accepts_native_text());
                    ensure!(workspace.plugins.credential_draft.is_some());
                    workspace.plugins.manager.selection = 0;
                    key(workspace, "enter", window, cx)?;
                    Ok(())
                })
            })??;
            wait(cx, handle, |workspace| {
                !workspace.plugins.credential_waiting
            })
            .await?;
            cx.update(|app| {
                handle.update(app, |workspace, window, cx| -> Result<()> {
                    let draft = workspace
                        .plugins
                        .credential_draft
                        .as_ref()
                        .context("Pending input was lost")?;
                    ensure!(*draft.value == marker);
                    ensure!(workspace.plugins.manager.feedback.is_some());
                    ensure!(fs::read_to_string(&index)? == "malformed fixture");
                    workspace.plugins.manager.selection = 2;
                    key(workspace, "enter", window, cx)?;
                    ensure!(workspace.plugins.credential_draft.is_none());
                    fs::write(&index, "{}")?;
                    open_prompt(workspace, &namespace);
                    workspace.replace_text_in_range(None, marker, window, cx);
                    key(workspace, "enter", window, cx)?;
                    workspace.plugins.manager.selection = 0;
                    key(workspace, "enter", window, cx)?;
                    Ok(())
                })
            })??;
            wait(cx, handle, |workspace| {
                !workspace.plugins.credential_waiting
            })
            .await?;
            let result = cx.update(|app| {
                handle.update(app, |workspace, _, _| -> Result<()> {
                    ensure!(workspace.plugins.credential_draft.is_none());
                    ensure!(
                        workspace.plugins.manager.feedback.as_deref() == Some("Credential saved")
                    );
                    ensure!(!fs::read_to_string(&index)?.contains(marker));
                    Ok(())
                })
            });
            cx.update(|app| {
                handle.update(app, |workspace, _, _| -> Result<()> {
                    workspace
                        .plugins
                        .session
                        .as_ref()
                        .context("Temporary session stopped")?
                        .send(nanobug::plugins::session::Action::DeleteCredential {
                            namespace: namespace.clone(),
                            slot: Some("login".into()),
                        })
                })
            })??;
            wait(cx, handle, |workspace| {
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .is_some_and(|session| session.view().message == "Saved credential removed")
            })
            .await?;
            result?
        },
    )
}

fn open_prompt(workspace: &mut Workspace, namespace: &str) {
    workspace.plugins.open = true;
    workspace.plugins.manager.prompt = Some(Input::new(
        Prompt::Credential {
            namespace: namespace.into(),
            slot: "login".into(),
            value: Zeroizing::new(String::with_capacity(2048)),
        },
        String::new(),
    ));
}

fn key(
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

async fn wait(
    cx: &mut gpui::AsyncApp,
    handle: WindowHandle<Workspace>,
    ready: impl Fn(&Workspace) -> bool,
) -> Result<()> {
    let expires = Instant::now() + Duration::from_secs(8);
    loop {
        if cx.update(|app| handle.update(app, |workspace, _, _| ready(workspace)))? {
            return Ok(());
        }
        ensure!(
            Instant::now() < expires,
            "Credential modal action did not settle"
        );
        cx.background_executor()
            .timer(Duration::from_millis(15))
            .await;
    }
}
