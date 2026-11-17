//! Restore plugin navigation across project openings and wait for published guest views.

use super::*;
use crate::grid::workspace::native_test;
use anyhow::{Result, ensure};
use nanobug::plugins::session::{Options, Surface};
use nanobug_plugin_sdk::GridSurface;
use std::{fs, sync::Arc};

/// Keep the user's plugin windows over a close dialog, without storing plugin text or prompt input.
#[test]
#[ignore = "requires the Windows DirectWrite and DirectX renderer"]
fn restores_plugin_windows_without_replaying_prompt_or_draft() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");
    fs::create_dir(&first)?;
    fs::create_dir(&second)?;
    let options = Options::take(
        &mut vec![
            "--plugin-profile".into(),
            fixture.path().join("profile").into_os_string(),
        ],
        fixture.path(),
    )?;
    native_test::run_async(
        first,
        options,
        |workspace| workspace.plugins.options = None,
        async move |handle, cx| {
            cx.update(|app| {
                handle.update(app, |workspace, _, _| -> Result<()> {
                    workspace.sync_project_plugin_windows();
                    let plugins = &mut workspace.plugins;
                    let old_scope = plugins.project_windows_scope.unwrap();
                    plugins.open = true;
                    plugins.manager.page = Page::Zip;
                    plugins.manager.search = "confirmed filter".into();
                    plugins.manager.selection = 4;
                    plugins.manager.open_search();
                    plugins.manager.prompt.as_mut().unwrap().value = "unconfirmed filter".into();
                    plugins.manager.live_search();

                    let old_key = nanobug::plugins::session::Key {
                        namespace: "example.canvas".into(),
                        instance: nanobug_plugin_sdk::Instance {
                            project: old_scope,
                            module: "main".into(),
                        },
                    };
                    plugins.surface.key = Some(old_key.clone());
                    plugins.surface.draft = "PLUGIN DRAFT".into();
                    plugins.freeze_project_windows_for_close();
                    plugins.open = false;
                    plugins.manager.selection = 0;
                    workspace.remember_project_plugin_windows();
                    let snapshot = &workspace.projects.active().plugin_windows;
                    let saved = serde_json::to_string(snapshot)?;
                    ensure!(
                        !saved.contains("PLUGIN DRAFT") && !saved.contains("unconfirmed filter")
                    );
                    let (next, added) = workspace.projects.add(second, None);
                    ensure!(added);
                    workspace.projects.project_mut(next).plugin_windows =
                        serde_json::from_str(&saved)?;
                    ensure!(workspace.projects.select(next));
                    workspace.sync_project_plugin_windows();
                    let plugins = &mut workspace.plugins;
                    ensure!(plugins.open && plugins.manager.page == Page::Zip);
                    ensure!(
                        plugins.manager.search == "confirmed filter"
                            && plugins.manager.selection == 4
                    );
                    ensure!(plugins.manager.prompt.is_none() && plugins.surface.key.is_none());
                    let new_key = plugins
                        .pending_project_surface
                        .as_ref()
                        .unwrap()
                        .key(plugins.project_windows_scope.unwrap());
                    ensure!(new_key.instance.project != old_scope);
                    let frame = Surface {
                        generation: 1,
                        revision: 1,
                        frame: Arc::new(GridSurface {
                            input: Some("Input".into()),
                            ..Default::default()
                        }),
                        drawing: None,
                        draft: "fresh host draft".into(),
                    };
                    Arc::make_mut(&mut plugins.view)
                        .surfaces
                        .insert(old_key, frame.clone());
                    workspace.reattach_project_surface();
                    ensure!(workspace.plugins.surface.key.is_none());
                    Arc::make_mut(&mut workspace.plugins.view)
                        .surfaces
                        .insert(new_key.clone(), frame);
                    workspace.reattach_project_surface();
                    ensure!(workspace.plugins.surface.key == Some(new_key));
                    ensure!(workspace.plugins.surface.draft == "fresh host draft");
                    Ok(())
                })
            })?
        },
    )
}
