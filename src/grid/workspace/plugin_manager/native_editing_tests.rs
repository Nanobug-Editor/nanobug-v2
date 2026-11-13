//! Verify editing commands through a native Workspace and actual plugin workers.

use super::{Detail, Workspace, comparison::Source, edits::PlanOwner, rows::Target};
use crate::grid::workspace::native_test;
use anyhow::{Context as _, Result, ensure};
use gpui::{AsyncApp, Context, KeyDownEvent, Keystroke, Window, WindowHandle};
use nanobug::plugins::{
    file_operations::{self, FileStepState},
    package::{Identity, InstallStore, Provider},
    session::{Action, Config, ConfigEdit, ConfigScope, Formatting, Key, Options, Session, State},
};
use nanobug_plugin_sdk::{FileOperation, Instance, Permission, PluginEvent};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Write,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

/// A small no-import module supplies replies only for its declared provider event.
struct Fixture {
    id: &'static str,
    event: &'static str,
    reply: Option<Value>,
    selected: Option<Value>,
    iterations: u32,
    active: bool,
    change_files: bool,
}

fn install(root: &Path, profile: &Path, fixture: Fixture) -> Result<Identity> {
    let event = format!("{}\",", fixture.event);
    let tag = i64::from_le_bytes(event.as_bytes()[..8].try_into()?);
    let offset = r#"{"event":{"event":""#.len();
    let encode = |value: Value, address: u64| -> Result<(String, u64)> {
        let bytes = serde_json::to_vec(&value)?;
        let escaped = bytes.iter().map(|byte| format!("\\{byte:02x}")).collect();
        Ok((escaped, (address << 32) | bytes.len() as u64))
    };
    let (plain, plain_ptr) = encode(fixture.reply.clone().unwrap_or_else(|| json!({})), 4096)?;
    let (selected, selected_ptr) =
        encode(fixture.selected.clone().unwrap_or_else(|| json!({})), 8192)?;
    let work = if fixture.iterations > 0 {
        format!(
            "(local.set $remaining (i32.const {}))
             (loop $work
                (local.tee $remaining (i32.sub (local.get $remaining) (i32.const 1)))
                br_if $work)",
            fixture.iterations
        )
    } else {
        String::new()
    };
    let response = if fixture.reply.is_none() {
        "unreachable".into()
    } else if fixture.selected.is_some() {
        let selection = r#"{"event":{"event":"format","selection":"#.len();
        format!(
            "(if (result i64)
                (i32.eq (i32.load8_u offset={selection} (local.get $input)) (i32.const 91))
                (then (i64.const {selected_ptr}))
                (else (i64.const {plain_ptr})))"
        )
    } else {
        format!("i64.const {plain_ptr}")
    };
    let wasm = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 20)
            (data (i32.const 16) "{{}}")
            (data (i32.const 4096) "{plain}")
            (data (i32.const 8192) "{selected}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 16384)
            (func (export "nb_free") (param i32 i32))
            (func (export "nb_run") (param $input i32) (param i32) (result i64)
                (local $remaining i32)
                (if (result i64)
                    (i64.eq (i64.load offset={offset} (local.get $input)) (i64.const {tag}))
                    (then {work} {response})
                    (else (i64.const 68719476738)))))"#
    ))?;
    let id = fixture.id;
    let archive = root.join(format!("{id}.zip"));
    let mut grants = BTreeSet::from([
        Permission::ReadDocument,
        Permission::EditDocument,
        Permission::ReadProject,
    ]);
    if fixture.change_files {
        grants.insert(Permission::ChangeFiles);
    }
    let permissions = serde_json::to_string(&grants)?;
    let manifest = format!(
        "name='Fixture Provider'\nid={id:?}\nversion='1.0.0'\n\
         permissions={permissions}\n\
         [engines]\nnanobug='*'\n[[modules]]\nid='language'\nwasm='main.wasm'\n\
         rule='rule.js'\nprovides=[{:?}]\n",
        fixture.event
    );
    let rule = if fixture.active {
        "return true;"
    } else {
        "return false;"
    };
    let mut zip = ZipWriter::new(File::create(&archive)?);
    for (path, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", wasm.as_slice()),
        ("rule.js", rule.as_bytes()),
    ] {
        zip.start_file(path, SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    let store = InstallStore::new(profile.to_path_buf());
    let record = store.install(store.prepare(&archive)?, &grants)?;
    Ok(record.release.manifest.identity())
}

pub(super) fn options(root: &Path, profile: &Path) -> Result<Options> {
    Options::take(
        &mut vec!["--plugin-profile".into(), profile.as_os_str().into()],
        root,
    )
}

/// Drive the native controller while its background workers deliver actual replies.
pub(super) fn step<T>(
    app: &AsyncApp,
    window: WindowHandle<Workspace>,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> Result<T>,
) -> Result<T> {
    app.update(|app| {
        window.update(app, |workspace, window, cx| {
            workspace.plugins.update_view();
            workspace.sync_plugin_projects();
            f(workspace, window, cx)
        })
    })?
}

pub(super) async fn until(
    app: &AsyncApp,
    window: WindowHandle<Workspace>,
    what: &str,
    mut ready: impl FnMut(&mut Workspace) -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if step(app, window, |workspace, _, _| ready(workspace))? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let details = step(app, window, |workspace, _, _| {
                Ok(format!(
                    "message {:?}, status {:?}, rows {:?}",
                    workspace.plugins.view.message,
                    workspace.projects.active().status,
                    workspace
                        .plugins
                        .rows()
                        .into_iter()
                        .map(|row| row.label)
                        .collect::<Vec<_>>()
                ))
            })?;
            anyhow::bail!("Timed out waiting for {what} ({details})");
        }
        app.background_executor()
            .timer(Duration::from_millis(5))
            .await;
    }
}

pub(super) fn use_session(workspace: &mut Workspace, session: Session) {
    workspace.plugins.options = None;
    workspace.plugins.session = Some(session);
    workspace.sync_plugin_projects();
}

/// Install a guest that returns its exact last UI input and callback count on Diagnose.
fn install_underline_guest(root: &Path, profile: &Path) -> Result<Identity> {
    use nanobug::plugins::package::{HostCompatibility, UI_CONTRACT};

    let scene = json!({"surface": {"title": "Native underlines", "canvas": {
        "draw": [{"op": "group", "offset": [7, 4], "draw": [{
            "op": "shaped_line", "origin": [2, 3], "text": "A中", "size": 15,
            "line_height": 20, "underline_tag": 23,
            "runs": [{"length": 4, "color": 4294967295_u32, "underline": {"thickness": 2}}],
        }]}],
        "interaction": {"next_frame": "report-underlines"},
    }}})
    .to_string();
    let escaped = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let prefix = r#"{"service_value":{"count":0,"input":"#;
    let response = 65_536_u64;
    let payload = response + prefix.len() as u64;
    let count = response + prefix.find('0').context("fixture callback count")? as u64;
    let result = (16_u64 << 32) | scene.len() as u64;
    let no_change = (4096_u64 << 32) | 2;
    let event = r#"{"event":{"event":""#.len();
    let wasm = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 20)
            (data (i32.const 16) "{}")
            (data (i32.const 4096) "{{}}")
            (data (i32.const {response}) "{}")
            (global $length (mut i32) (i32.const {}))
            (func (export "nb_alloc") (param i32) (result i32) i32.const 16384)
            (func (export "nb_free") (param i32 i32))
            (func (export "nb_run") (param $input i32) (param $size i32) (result i64)
                (local $event i32)
                (local.set $event (i32.load8_u offset={event} (local.get $input)))
                (if (i32.eq (local.get $event) (i32.const 111))
                    (then (return (i64.const {result}))))
                (if (i32.eq (local.get $event) (i32.const 117))
                    (then
                        (i32.store8 (i32.const {count}) (i32.add (i32.load8_u (i32.const {count})) (i32.const 1)))
                        (memory.copy (i32.const {payload}) (local.get $input) (local.get $size))
                        (i32.store16 (i32.add (i32.const {payload}) (local.get $size)) (i32.const 32125))
                        (global.set $length (i32.add (local.get $size) (i32.const {})))))
                (if (i32.eq (local.get $event) (i32.const 100))
                    (then (return (i64.or (i64.const {}) (i64.extend_i32_u (global.get $length))))))
                i64.const {no_change}))"#,
        escaped(&scene),
        escaped(&format!("{prefix}null}}}}")),
        prefix.len() + 6,
        prefix.len() + 2,
        response << 32,
    ))?;
    let manifest = format!(
        "name='Underline Fixture'\nid='underline-fixture'\nversion='1.0.0'\n[engines]\nnanobug='*'\n\
         [experiments]\nui={UI_CONTRACT:?}\n[[modules]]\nid='underlines'\nwasm='main.wasm'\ninteractive=true\n"
    );
    let archive = root.join("underlines.zip");
    let mut zip = ZipWriter::new(File::create(&archive)?);
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    let store =
        InstallStore::with_compatibility(profile.to_path_buf(), HostCompatibility::current(true));
    let record = store.install(store.prepare(&archive)?, &BTreeSet::new())?;
    Ok(record.release.manifest.identity())
}

#[test]
#[ignore = "requires the Windows native renderer and locally built plugin runtime"]
fn native_shaped_line_reports_geometry_once_per_scene() -> Result<()> {
    use gpui::AppContext;
    use nanobug_plugin_sdk::{Input, canvas::LineUnderlines};

    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "Native underline geometry")?;
    let profile = temp.path().join("profile");
    let namespace = install_underline_guest(temp.path(), &profile)?.namespace();
    let development = || {
        Options::take(
            &mut vec!["--plugin-dev-profile".into(), profile.as_os_str().into()],
            temp.path(),
        )
    };
    let editor = Session::start(development()?).0;
    native_test::run_async(
        project,
        development()?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            until(app, window, "prepared underline scene", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .surfaces
                    .keys()
                    .any(|key| key.namespace == namespace))
            })
            .await?;
            let (owner, revision, completed) = step(app, window, |workspace, native, cx| {
                let (owner, scene) = workspace
                    .plugins
                    .view
                    .surfaces
                    .iter()
                    .find(|(key, _)| key.namespace == namespace)
                    .context("fixture scene")?;
                let owner = owner.clone();
                let revision = scene.revision;
                let completed = workspace.plugins.view.runtime_status[&namespace].completed;
                workspace.plugins.surface = Some(owner.clone());
                workspace.sync(native, cx);
                Ok((owner, revision, completed))
            })?;
            let draw = |app: &AsyncApp, scale: Option<f32>| -> Result<()> {
                app.update(|app| {
                    app.update_window(window.into(), |_, native, cx| {
                        if let Some(scale) = scale {
                            native.set_scale_factor(scale);
                        }
                        native.refresh();
                        native.draw(cx).clear(cx);
                    })
                })?;
                Ok(())
            };
            let read = async |app: &AsyncApp| -> Result<Value> {
                let reply = step(app, window, |workspace, _, _| {
                    let (send, reply) = async_channel::bounded(1);
                    workspace
                        .plugins
                        .session
                        .as_ref()
                        .context("fixture session")?
                        .send(Action::Invoke {
                            key: owner.clone(),
                            event: PluginEvent::Diagnose,
                            reply: Some(send),
                            timeout: Duration::from_secs(3),
                        })?;
                    Ok(reply)
                })?;
                reply
                    .recv()
                    .await?
                    .map_err(anyhow::Error::msg)?
                    .output
                    .service_value
                    .context("fixture guest report")
            };
            draw(app, None)?;
            until(
                app,
                window,
                "first viewport and paint callbacks",
                |workspace| {
                    Ok(
                        workspace.plugins.view.runtime_status[&namespace].completed
                            >= completed + 2,
                    )
                },
            )
            .await?;
            let initial = read(app).await?;
            ensure!(
                initial["count"] == 1,
                "first native paint report: {initial}"
            );
            let input: Input = serde_json::from_value(initial["input"].clone())?;
            let viewport = input.viewport.context("guest viewport")?;
            ensure!(viewport.shaped_line);
            let PluginEvent::Ui { action, argument } = input.event else {
                anyhow::bail!("expected Canvas UI callback");
            };
            ensure!(action == "report-underlines" && argument["event"] == "next_frame");
            let reports: Vec<LineUnderlines> =
                serde_json::from_value(argument["underlines"].clone())?;
            ensure!(reports.len() == 1 && reports[0].tag == 23 && reports[0].segments.len() == 1);
            let line = &reports[0].segments[0];
            ensure!(
                line.range == [0, 4]
                    && line.origin[0] == 9.
                    && line.origin[1] > 7.
                    && line.width > 0.
                    && line.thickness == 2.
            );

            draw(app, Some(viewport.scale + 0.25))?;
            draw(app, None)?;
            step(app, window, |workspace, _, _| {
                ensure!(workspace.plugins.viewport.context("resized viewport")?.id > viewport.id);
                ensure!(workspace.plugins.view.surfaces[&owner].revision == revision);
                Ok(())
            })?;
            ensure!(
                read(app).await? == initial,
                "same revision emitted a second report"
            );
            Ok(())
        },
    )
}

#[test]
#[ignore = "requires native Windows GPUI and the bundled debug plugin runtime"]
fn native_config_comparison_shows_full_text_and_keeps_apply_explicit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    let path = project.join(".nb/plugins.json");
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(project.join("README.md"), "Configuration comparison")?;
    let owner = install(
        temp.path(),
        &profile,
        Fixture {
            id: "compare-fixture",
            event: "format",
            reply: Some(json!({})),
            selected: None,
            iterations: 0,
            active: false,
            change_files: false,
        },
    )?;
    let make_config = |text: &str| {
        let mut config = Config::default();
        let plugin = config.plugin_mut(owner.clone());
        plugin.settings.insert("description".into(), json!(text));
        config
    };
    let initial = make_config("original");
    let local_text = format!("{}LOCAL-END", "长👩‍💻".repeat(150));
    let external_text = format!("{}EXTERNAL-END", "外👨‍💻".repeat(160));
    let local = make_config(&local_text);
    let mut external = make_config(&external_text);
    external
        .plugin_mut(Identity {
            name: "Another plugin".into(),
            id: "another-plugin".into(),
        })
        .settings
        .insert("second-owner".into(), json!("kept in scope"));
    let external_bytes = serde_json::to_vec(&external)?;
    let mut latest = external.clone();
    latest
        .plugin_mut(owner.clone())
        .settings
        .insert("newer".into(), json!(true));
    fs::write(&path, serde_json::to_vec(&initial)?)?;
    let preserved_path = project
        .join("保留👩‍💻".repeat(30))
        .join("raw-configuration.json");
    let editor = Session::start(options(temp.path(), &profile)?).0;
    native_test::run_async(
        project,
        options(temp.path(), &profile)?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            let namespace = owner.namespace();
            let scope = step(app, window, |workspace, _, _| {
                Ok(ConfigScope::Project(
                    workspace.projects.active().plugin_scope,
                ))
            })?;
            let press = |workspace: &mut Workspace,
                         key: &'static str,
                         native: &mut Window,
                         cx: &mut Context<Workspace>|
             -> Result<()> {
                workspace.key_down(
                    &KeyDownEvent {
                        keystroke: Keystroke::parse(key)?,
                        is_held: false,
                        prefer_character_input: false,
                    },
                    native,
                    cx,
                );
                Ok(())
            };
            until(app, window, "initial configuration", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .is_some_and(|config| config.applied == initial))
            })
            .await?;
            step(app, window, |workspace, _, _| {
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .unwrap()
                    .send(Action::StageConfig(scope, local.clone()))
            })?;
            fs::write(&path, &external_bytes)?;
            until(app, window, "both conflicting versions", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .is_some_and(|config| {
                        config.external.as_ref() == Some(&external)
                            && config.draft.as_ref() == Some(&local)
                    }))
            })
            .await?;
            let origin = step(app, window, |workspace, native, cx| {
                workspace.open_plugin_manager(native, cx);
                let manager = &mut workspace.plugins.manager;
                manager.detail = Some(namespace.clone());
                manager.detail_page = Detail::Settings;
                let origin = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| matches!(row.target, Target::Detail(Detail::Compare)))
                    .context("Compare row")?;
                workspace.plugins.manager.selection = origin;
                press(workspace, "enter", native, cx)?;
                ensure!(workspace.plugins.manager.detail_page == Detail::Compare);
                for (source, text) in [
                    (Source::Local, &local_text),
                    (Source::External, &external_text),
                ] {
                    let index = workspace.plugins.rows().iter().position(|row| matches!(&row.target, Target::Compare(candidate) if *candidate == source)).context("Version row")?;
                    workspace.plugins.manager.selection = index;
                    press(workspace, "enter", native, cx)?;
                    let area = workspace.plugin_manager_area();
                    let preview = workspace.comparison_preview().context("Active preview")?;
                    let lines = preview.lines(area.w.saturating_sub(4), &workspace.spacing);
                    let content = lines
                        .iter()
                        .map(|range| preview.line(range.clone()))
                        .collect::<String>();
                    ensure!(
                        content.contains(text.as_str()),
                        "Wrapped content lost characters"
                    );
                    if matches!(source, Source::External) {
                        ensure!(
                            content.contains("second-owner"),
                            "Another plugin's settings are also affected by Resolve"
                        );
                    }
                    let count = lines.len();
                    drop(lines);
                    ensure!(count > area.h - super::LIST_START - 5);
                    press(workspace, "end", native, cx)?;
                    ensure!(workspace.plugins.manager.selection == count - 1);
                    let (x, y, _, insert, shown) =
                        workspace.plugin_caret().context("Preview caret")?;
                    ensure!(shown && !insert && area.contains(x as usize, y as usize));
                    press(workspace, "k", native, cx)?;
                    ensure!(workspace.plugins.manager.selection == count - 2);
                    press(workspace, "home", native, cx)?;
                    ensure!(workspace.plugins.manager.selection == 0);
                    press(workspace, "q", native, cx)?;
                    ensure!(workspace.plugins.manager.selection == index);
                }
                // Simulate the published sidecar metadata without changing the pinned real draft.
                let comparison = workspace.plugins.manager.comparison.as_mut().unwrap();
                let config = Arc::make_mut(&mut comparison.configurations)
                    .get_mut(&scope)
                    .unwrap();
                config.preserved_external = Some(preserved_path.clone());
                let preserved = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| matches!(row.target, Target::Compare(Source::Preserved)))
                    .context("Preserved file row")?;
                workspace.plugins.manager.selection = preserved;
                press(workspace, "enter", native, cx)?;
                let preview = workspace
                    .comparison_preview()
                    .context("Preserved file preview")?;
                let width = workspace.plugin_manager_area().w.saturating_sub(4);
                let displayed = preview
                    .lines(width, &workspace.spacing)
                    .iter()
                    .map(|range| preview.line(range.clone()))
                    .collect::<String>();
                ensure!(displayed == preserved_path.display().to_string());
                press(workspace, "escape", native, cx)?;
                ensure!(workspace.plugins.manager.selection == preserved);
                Ok(origin)
            })?;
            let choose_external = |workspace: &mut Workspace,
                                   native: &mut Window,
                                   cx: &mut Context<Workspace>|
             -> Result<()> {
                workspace.plugins.manager.selection = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| {
                        matches!(
                            row.target,
                            Target::Action(Action::ResolveConfig(_, _, true))
                        )
                    })
                    .context("Resolution row")?;
                press(workspace, "enter", native, cx)
            };
            fs::write(&path, serde_json::to_vec(&latest)?)?;
            until(app, window, "second external version", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .and_then(|config| config.external.as_ref())
                    == Some(&latest))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                choose_external(workspace, native, cx)?;
                ensure!(
                    workspace.plugins.manager.detail_page == Detail::Settings
                        && workspace.plugins.manager.selection == origin
                );
                ensure!(
                    workspace.plugins.manager.feedback.as_deref()
                        == Some(nanobug::i18n::text("plugin_manager.compare_changed_again"))
                );
                ensure!(
                    workspace.plugins.view.configurations[&scope].draft.as_ref() == Some(&local)
                );
                Ok(())
            })?;
            fs::write(&path, &external_bytes)?;
            until(app, window, "external version to use", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .and_then(|config| config.external.as_ref())
                    == Some(&external))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.plugins.manager.selection = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| matches!(row.target, Target::Detail(Detail::Compare)))
                    .context("Compare row")?;
                press(workspace, "enter", native, cx)?;
                choose_external(workspace, native, cx)?;
                ensure!(workspace.plugins.manager.detail_page == Detail::Settings);
                Ok(())
            })?;
            until(
                app,
                window,
                "resolution stages the external version",
                |workspace| {
                    Ok(workspace
                        .plugins
                        .view
                        .configurations
                        .get(&scope)
                        .is_some_and(|config| {
                            config.applied == initial
                                && config.draft.as_ref() == Some(&external)
                                && config.external.is_none()
                        }))
                },
            )
            .await?;
            ensure!(
                fs::read(&path)? == external_bytes,
                "Resolution rewrote the file before Apply"
            );
            step(app, window, |workspace, native, cx| {
                workspace.plugins.manager.selection = workspace
                    .plugins
                    .rows()
                    .iter()
                    .position(|row| matches!(row.target, Target::Action(Action::ApplyConfig(_))))
                    .context("Apply row")?;
                press(workspace, "enter", native, cx)
            })?;
            until(app, window, "explicit Apply", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .is_some_and(|config| config.applied == external && config.draft.is_none()))
            })
            .await
        },
    )
}

#[test]
#[ignore = "requires native GPUI, the debug worker and NANOBUG_PLUGIN_TEST_ARCHIVE"]
fn native_plugin_composition_and_paste_survive_a_worker_exit() -> Result<()> {
    use gpui::{ClipboardItem, EntityInputHandler};
    use nanobug::plugins::package::HostCompatibility;

    let archive = std::env::var_os("NANOBUG_PLUGIN_TEST_ARCHIVE").context("Select a UI SDK ZIP")?;
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let document = project.join("README.md");
    fs::write(&document, "saved正文\n")?;
    let profile = temp.path().join("profile");
    let store = InstallStore::with_compatibility(profile.clone(), HostCompatibility::current(true));
    let package = store.prepare(Path::new(&archive))?;
    let grants = package.release.manifest.permissions.clone();
    let installed = store.install(package, &grants)?;
    let namespace = installed.release.manifest.identity().namespace();
    let development = || {
        Options::take(
            &mut vec!["--plugin-dev-profile".into(), profile.as_os_str().into()],
            temp.path(),
        )
    };
    let editor = Session::start(development()?).0;
    native_test::run_async(
        project,
        development()?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            until(
                app,
                window,
                "installed native Canvas surface",
                |workspace| {
                    Ok(workspace
                        .plugins
                        .view
                        .surfaces
                        .keys()
                        .any(|key| key.namespace == namespace))
                },
            )
            .await?;
            let (key, pid, generation, before) = step(app, window, |workspace, native, cx| {
                let (key, surface) = workspace
                    .plugins
                    .view
                    .surfaces
                    .iter()
                    .find(|(key, _)| key.namespace == namespace)
                    .context("Canvas surface")?;
                let (key, generation, before) =
                    (key.clone(), surface.generation, surface.draft.clone());
                let pid = workspace.plugins.view.workers[&namespace];
                workspace.plugins.surface = Some(key.clone());
                workspace.plugins.surface_focus = true;
                workspace.plugins.draft.clone_from(&before);
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active]
                    .vim
                    .doc
                    .replace(0..0, "unsaved ", true);
                workspace.replace_and_mark_text_in_range(None, "ni", Some(2..2), native, cx);
                ensure!(workspace.plugins.draft == before);
                workspace.replace_text_in_range(None, "你", native, cx);
                Ok((key, pid, generation, format!("{before}你")))
            })?;
            until(
                app,
                window,
                "confirmed candidate before worker exit",
                |workspace| {
                    Ok(workspace
                        .plugins
                        .view
                        .surfaces
                        .get(&key)
                        .is_some_and(|surface| surface.draft == before))
                },
            )
            .await?;
            let stopped = std::process::Command::new("taskkill.exe")
                .args(["/F", "/PID", &pid.to_string()])
                .output()?;
            ensure!(stopped.status.success(), "Could not stop fixture worker");
            until(app, window, "worker recovery delay", |workspace| {
                Ok(!workspace.plugins.view.workers.contains_key(&namespace))
            })
            .await?;
            let expected = format!("{before}中文👩‍💻");
            step(app, window, |workspace, native, cx| {
                ensure!(
                    workspace.accepts_native_text(),
                    "Recovering panel lost its text focus"
                );
                workspace.replace_and_mark_text_in_range(None, "zhong", Some(5..5), native, cx);
                ensure!(workspace.plugins.draft == before);
                workspace.replace_text_in_range(None, "中", native, cx);
                workspace.paste(ClipboardItem::new_string("文👩‍💻".into()), native, cx);
                ensure!(workspace.plugins.draft == expected);
                Ok(())
            })?;
            until(
                app,
                window,
                "replacement worker receives confirmed draft",
                |workspace| {
                    Ok(workspace
                        .plugins
                        .view
                        .surfaces
                        .get(&key)
                        .is_some_and(|surface| {
                            surface.generation != generation && surface.draft == expected
                        }))
                },
            )
            .await?;
            step(app, window, |workspace, _, _| {
                ensure!(workspace.projects.active().editor.vim().doc.text == "unsaved saved正文\n");
                ensure!(fs::read_to_string(document)? == "saved正文\n");
                Ok(())
            })
        },
    )
}

#[test]
#[ignore = "requires Windows native GPUI and the bundled debug plugin runtime"]
fn native_language_panel_handles_fast_slow_failed_and_stale_providers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "文档\n")?;
    let mut slow = None;
    for (id, reply, iterations, active) in [
        (
            "fast",
            Some(json!({"completions":["zeta","zeta"]})),
            0,
            true,
        ),
        (
            "slow",
            Some(json!({"completions":["alpha","zeta"]})),
            9_000_000,
            true,
        ),
        ("failed", None, 0, true),
        (
            "inactive",
            Some(json!({"completions":["unwanted"]})),
            0,
            false,
        ),
    ] {
        let identity = install(
            temp.path(),
            &profile,
            Fixture {
                id,
                event: "complete",
                reply,
                selected: None,
                iterations,
                active,
                change_files: false,
            },
        )?;
        if id == "slow" {
            slow = Some(identity.namespace());
        }
    }
    let slow = slow.context("Missing slow provider fixture")?;
    let editor = Session::start(options(temp.path(), &profile)?).0;
    native_test::run_async(
        project,
        options(temp.path(), &profile)?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            until(
                app,
                window,
                "three eligible provider processes",
                |workspace| {
                    Ok(workspace.plugins.view.workers.len() == 3
                        && workspace
                            .plugins
                            .view
                            .modules
                            .values()
                            .filter(|state| **state == State::Ready)
                            .count()
                            == 3
                        && workspace
                            .plugins
                            .view
                            .modules
                            .values()
                            .any(|state| *state == State::Unmatched))
                },
            )
            .await?;
            step(app, window, |workspace, native, cx| {
                let project = workspace
                    .projects
                    .active_project()
                    .context("Missing project")?;
                let key = Key {
                    namespace: slow.clone(),
                    instance: Instance {
                        project: project.plugin_scope,
                        module: "language".into(),
                    },
                };
                let session = workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Missing session")?;
                for _ in 0..10 {
                    session.send(Action::Invoke {
                        key: key.clone(),
                        event: PluginEvent::Complete { offset: 0 },
                        reply: None,
                        timeout: Duration::from_secs(3),
                    })?;
                }
                workspace.plugin_language(Provider::Complete, native, cx);
                Ok(())
            })?;
            until(app, window, "fast completion", |workspace| {
                let rows = workspace.plugins.rows();
                Ok(rows.iter().any(|row| row.label == "zeta")
                    && rows.iter().any(|row| row.label == "Waiting for providers…"))
            })
            .await?;
            until(app, window, "all provider replies", |workspace| {
                let rows = workspace.plugins.rows();
                let Some(row) = rows.iter().find(|row| row.label == "zeta") else {
                    return Ok(false);
                };
                if rows.iter().any(|row| row.label == "Waiting for providers…") {
                    ensure!(
                        row.description.contains("(fast)"),
                        "The pending result came from the wrong provider"
                    );
                    return Ok(false);
                }
                ensure!(
                    rows.len() == 2
                        && row.description.contains("(fast)")
                        && row.description.contains("(slow)"),
                    "Duplicate or missing provider results"
                );
                let selected = &rows[workspace.plugins.manager.selection];
                ensure!(
                    matches!(&selected.target, Target::Completion(text) if text == "zeta"),
                    "A late result changed the selected completion"
                );
                Ok(true)
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.plugin_back();
                workspace.plugin_language(Provider::Complete, native, cx);
                let editor = &mut workspace
                    .projects
                    .active_project_mut()
                    .context("Missing project")?
                    .editor;
                editor.buffers[editor.active]
                    .vim
                    .doc
                    .replace(0..0, "new ", true);
                Ok(())
            })?;
            until(app, window, "stale result rejection", |workspace| {
                Ok(workspace.plugins.rows().is_empty())
            })
            .await?;
            Ok(())
        },
    )
}

#[test]
#[ignore = "requires Windows native GPUI and the bundled debug plugin runtime"]
fn native_formatting_checks_selection_undo_cancel_and_save_join() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    let document = project.join("README.md");
    fs::create_dir(&project)?;
    fs::write(&document, "ab cd\n")?;
    install(
        temp.path(),
        &profile,
        Fixture {
            id: "formatter",
            event: "format",
            reply: Some(
                json!({"edits":[{"start":0,"end":2,"text":"AB"},{"start":3,"end":5,"text":"CD"}]}),
            ),
            selected: Some(json!({"edits":[{"start":3,"end":5,"text":"CD"}]})),
            iterations: 0,
            active: true,
            change_files: false,
        },
    )?;
    let editor = Session::start(options(temp.path(), &profile)?).0;
    native_test::run_async(
        project,
        options(temp.path(), &profile)?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            until(app, window, "ready formatter", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .modules
                    .values()
                    .any(|state| *state == State::Ready))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.manual_format(None, native, cx);
                workspace.save_with_plugins(None, false, native, cx);
                Ok(())
            })?;
            until(
                app,
                window,
                "manual format with saving disabled",
                |workspace| Ok(workspace.projects.active().editor.vim().doc.text == "AB CD\n"),
            )
            .await?;
            ensure!(
                fs::read_to_string(&document)? == "ab cd\n",
                "An ordinary save adopted a still-running manual result"
            );
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                let doc = &mut editor.buffers[editor.active].vim.doc;
                doc.undo(false);
                ensure!(
                    doc.text == "ab cd\n",
                    "The complete group was not undone together"
                );
                workspace.manual_format(Some(3..5), native, cx);
                Ok(())
            })?;
            until(app, window, "selection formatting", |workspace| {
                Ok(workspace.projects.active().editor.vim().doc.text == "ab CD\n")
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active].vim.doc.undo(false);
                workspace.manual_format(None, native, cx);
                workspace.cancel_format(native, cx);
                ensure!(
                    workspace.projects.active().status == "Formatting cancelled",
                    "Cancellation was not reported"
                );
                workspace.manual_format(None, native, cx);
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active]
                    .vim
                    .doc
                    .replace(0..0, "new ", true);
                Ok(())
            })?;
            until(app, window, "stale format reply", |workspace| {
                let project = workspace.projects.active();
                Ok((project
                    .status
                    .to_ascii_lowercase()
                    .contains("document changed")
                    || project.status == "Plugin result is out of date")
                    && project.editor.vim().doc.text == "new ab cd\n")
            })
            .await?;
            let scope = step(app, window, |workspace, _, _| {
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active].vim.doc.undo(false);
                let scope = ConfigScope::Project(workspace.projects.active().plugin_scope);
                let session = workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Missing session")?;
                session.send(Action::EditConfig(
                    scope,
                    ConfigEdit::ToggleFormatOnSave("md".into()),
                ))?;
                session.send(Action::ApplyConfig(scope))?;
                Ok(scope)
            })?;
            until(app, window, "format-on-save Apply", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .and_then(|view| view.applied.formatters.get("md"))
                    .is_some_and(|format| format.on_save == Some(true)))
            })
            .await?;
            let revision = step(app, window, |workspace, native, cx| {
                let revision = workspace.projects.active().editor.vim().doc.revision;
                workspace.manual_format(None, native, cx);
                workspace.save_with_plugins(None, false, native, cx);
                Ok(revision)
            })?;
            until(
                app,
                window,
                "one format result shared with the save",
                |workspace| {
                    let doc = &workspace.projects.active().editor.vim().doc;
                    Ok(doc.text == "AB CD\n"
                        && doc.revision == revision + 1
                        && fs::read_to_string(&document)? == "AB CD\n")
                },
            )
            .await?;
            Ok(())
        },
    )
}

fn set_formatting(
    workspace: &Workspace,
    scope: ConfigScope,
    provider: Option<Identity>,
) -> Result<()> {
    let session = workspace
        .plugins
        .session
        .as_ref()
        .context("Missing session")?;
    let mut config = Config::default();
    config.formatters.insert(
        "md".into(),
        Formatting {
            provider,
            on_save: Some(true),
        },
    );
    session.send(Action::StageConfig(scope, config))?;
    session.send(Action::ApplyConfig(scope))
}

#[test]
#[ignore = "requires Windows native GPUI and the bundled debug plugin runtime"]
fn native_formatter_defaults_use_project_identity_and_never_prompt_during_save() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    let document = project.join("README.md");
    fs::create_dir(&project)?;
    fs::write(&document, "ab cd\n")?;
    let formatter = |id, text| {
        install(
            temp.path(),
            &profile,
            Fixture {
                id,
                event: "format",
                reply: Some(json!({"edits":[{"start":0,"end":2,"text":text}]})),
                selected: None,
                iterations: 0,
                active: true,
                change_files: false,
            },
        )
    };
    let global = formatter("global", "ONE")?;
    let local = formatter("local", "TWO")?;
    let editor = Session::start(options(temp.path(), &profile)?).0;
    native_test::run_async(
        project,
        options(temp.path(), &profile)?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            until(app, window, "two ready formatters", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .modules
                    .values()
                    .filter(|state| **state == State::Ready)
                    .count()
                    == 2)
            })
            .await?;
            let scope = step(app, window, |workspace, _, _| {
                ensure!(
                    workspace.plugins.formatting.choices.is_none(),
                    "Startup opened a formatter picker"
                );
                let scope = ConfigScope::Project(workspace.projects.active().plugin_scope);
                set_formatting(workspace, scope, None)?;
                Ok(scope)
            })?;
            until(app, window, "save policy", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .configurations
                    .get(&scope)
                    .and_then(|view| view.applied.formatters.get("md"))
                    .is_some_and(|format| format.on_save == Some(true)))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.save_with_plugins(None, false, native, cx);
                ensure!(
                    workspace.plugins.formatting.choices.is_none(),
                    "Save opened a formatter picker"
                );
                ensure!(
                    workspace
                        .projects
                        .active()
                        .status
                        .contains("Choose a default formatter"),
                    "Save did not explain the missing default"
                );
                ensure!(
                    fs::read_to_string(&document)? == "ab cd\n",
                    "Save unexpectedly formatted the document"
                );
                workspace.manual_format(None, native, cx);
                let rows = workspace.plugins.rows();
                ensure!(
                    rows.len() == 2,
                    "First manual format did not offer both providers"
                );
                let selected = rows
                    .into_iter()
                    .find_map(|row| match row.target {
                        Target::Formatter(key) if key.namespace == global.namespace() => Some(key),
                        _ => None,
                    })
                    .context("Missing global provider choice")?;
                workspace.choose_formatter(selected, native, cx);
                Ok(())
            })?;
            until(app, window, "chosen formatter result", |workspace| {
                Ok(workspace.projects.active().editor.vim().doc.text == "ONE cd\n")
            })
            .await?;
            step(app, window, |workspace, _, _| {
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active].vim.doc.undo(false);
                set_formatting(workspace, ConfigScope::AllProjects, Some(global.clone()))?;
                set_formatting(workspace, scope, Some(local.clone()))
            })?;
            until(app, window, "project and global defaults", |workspace| {
                let configurations = &workspace.plugins.view.configurations;
                Ok([(ConfigScope::AllProjects, &global), (scope, &local)]
                    .iter()
                    .all(|(scope, expected)| {
                        configurations
                            .get(scope)
                            .and_then(|view| view.applied.formatters.get("md"))
                            .and_then(|format| format.provider.as_ref())
                            == Some(*expected)
                    }))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.manual_format(None, native, cx);
                Ok(())
            })?;
            until(app, window, "project override result", |workspace| {
                Ok(workspace.projects.active().editor.vim().doc.text == "TWO cd\n")
            })
            .await?;
            step(app, window, |workspace, _, _| {
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active].vim.doc.undo(false);
                workspace
                    .plugins
                    .session
                    .as_ref()
                    .context("Missing session")?
                    .send(Action::SetEnabled(local.namespace(), false))
            })?;
            until(app, window, "unavailable selected formatter", |workspace| {
                Ok(workspace.plugins.view.modules.iter().any(|(key, state)| {
                    key.namespace == local.namespace() && *state == State::Disabled
                }))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                workspace.manual_format(None, native, cx);
                ensure!(
                    workspace.projects.active().status == "No applicable formatter is ready",
                    "The host did not report the unavailable default"
                );
                ensure!(
                    workspace.projects.active().editor.vim().doc.text == "ab cd\n",
                    "The host substituted a different plugin"
                );
                ensure!(
                    workspace.plugins.formatting.choices.is_none(),
                    "An unavailable explicit default opened a new picker"
                );
                Ok(())
            })
        },
    )
}

#[test]
#[ignore = "requires Windows native GPUI and the bundled debug plugin runtime"]
fn native_file_plan_protects_dirty_buffers_and_consumes_one_confirmation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profile = temp.path().join("profile");
    let project = temp.path().join("workspace");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "body\n")?;
    let identity = install(
        temp.path(),
        &profile,
        Fixture {
            id: "file-operations",
            event: "complete",
            reply: Some(json!({})),
            selected: None,
            iterations: 0,
            active: true,
            change_files: true,
        },
    )?;
    let namespace = identity.namespace();
    let editor = Session::start(options(temp.path(), &profile)?).0;
    native_test::run_async(
        project.clone(),
        options(temp.path(), &profile)?,
        move |workspace| use_session(workspace, editor),
        async move |window, app| {
            until(app, window, "file operations module", |workspace| {
                Ok(workspace
                    .plugins
                    .view
                    .modules
                    .values()
                    .any(|state| *state == State::Ready))
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let owner = PlanOwner {
                    key: Key {
                        namespace: namespace.clone(),
                        instance: Instance {
                            project: workspace.projects.active().plugin_scope,
                            module: "language".into(),
                        },
                    },
                    digest: workspace.plugins.view.installed[0].release.digest.clone(),
                    generation: workspace.plugins.view.runtime_status[&namespace].generation,
                };
                workspace.plugins.request_id += 1;
                workspace.preview_file_operations(
                    owner,
                    workspace.plugins.request_id,
                    vec![
                        FileOperation::Move {
                            from: "README.md".into(),
                            to: "renamed.md".into(),
                        },
                        FileOperation::Create {
                            path: "created.txt".into(),
                            text: "once".into(),
                        },
                    ],
                    native,
                    cx,
                );
                Ok(())
            })?;
            until(app, window, "file operation preview", |workspace| {
                Ok(workspace.plugins.file_plan.is_some())
            })
            .await?;
            step(app, window, |workspace, native, cx| {
                let rows = workspace.plugins.rows();
                ensure!(
                    rows.len() == 3 && matches!(rows[0].target, Target::ApplyFiles),
                    "The native preview did not show the complete plan"
                );
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active]
                    .vim
                    .doc
                    .replace(0..0, "unsaved ", true);
                workspace.apply_plugin_files(native, cx);
                ensure!(
                    workspace.plugins.file_plan.is_some()
                        && workspace.plugins.manager.feedback.as_deref()
                            == Some("Save affected buffers before applying file operations"),
                    "The dirty buffer was not protected"
                );
                ensure!(
                    !project.join("renamed.md").exists(),
                    "The rejected plan changed a file"
                );
                let editor = &mut workspace.projects.active_mut().editor;
                editor.buffers[editor.active].vim.doc.undo(false);
                workspace.apply_plugin_files(native, cx);
                workspace.apply_plugin_files(native, cx);
                Ok(())
            })?;
            until(app, window, "file operation receipt", |workspace| {
                Ok(!workspace.plugins.file_busy && workspace.plugins.file_report.len() == 2)
            })
            .await?;
            let receipts = file_operations::review(&profile, &namespace)?;
            ensure!(
                receipts.len() == 1
                    && receipts[0]
                        .steps
                        .iter()
                        .all(|step| step.state == FileStepState::Done),
                "The confirmed file plan did not execute exactly once"
            );
            ensure!(
                fs::read_to_string(project.join("renamed.md"))? == "body\n"
                    && fs::read_to_string(project.join("created.txt"))? == "once"
                    && !project.join("README.md").exists(),
                "The file operation results did not match the preview"
            );
            Ok(())
        },
    )
}
