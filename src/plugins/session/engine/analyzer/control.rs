//! Explicit language-server transitions on the plugin supervisor.

use super::{Analyzer, Binding, Engine, Key, PathBuf};
use crate::plugins::session::{
    LanguageServerConfig, LanguageServerOperation, LanguageServerPhase, LanguageServerTarget,
    language_server_configs,
};
use std::collections::BTreeMap;

/// Prevent ordinary auto-attachment following a user-issued stop or disable.
#[derive(PartialEq, Eq)]
enum Suspension {
    /// Retain the originating document so an unqualified restart can find a stopped instance.
    Stopped(Option<(u64, PathBuf)>),
    /// Explicitly disabled throughout this Session until the user enables it.
    Disabled,
}

/// A command awaiting the retirement of every selected old process.
struct Transition {
    operation: LanguageServerOperation,
    keys: Vec<Key>,
    reply: async_channel::Sender<Result<usize, String>>,
}

/// Session-local server policy, shutting-down workers, and asynchronous acknowledgements.
#[derive(Default)]
pub(in crate::plugins::session::engine) struct ServerControls {
    suspended: BTreeMap<Key, Suspension>,
    retiring: BTreeMap<Key, Analyzer>,
    pending: Vec<Transition>,
}

impl ServerControls {
    /// Preserve tombstones for stopped servers and retire only registered children that have exited.
    pub(super) fn retain(&mut self, project_open: impl Fn(u64) -> bool) {
        self.suspended
            .retain(|key, _| project_open(key.instance.project));
        self.retiring.retain(|_, server| !server.has_stopped());
    }

    /// A replacement may start only after any former worker has finished.
    pub(super) fn can_attach(&self, key: &Key) -> bool {
        !self.suspended.contains_key(key) && !self.retiring.contains_key(key)
    }

    /// Retire a superseded `binding`, releasing its promises before starting the next instance.
    pub(super) fn retire(&mut self, key: Key, mut binding: Binding) {
        if let Some(server) = binding.server.take() {
            server.request_stop();
            if !server.has_stopped() {
                self.retiring.insert(key, server);
            }
        }
    }

    /// Keep an explicitly replaced service alive even if focus moved during teardown.
    pub(super) fn restart_requested(&self, key: &Key) -> bool {
        self.pending.iter().any(|transition| {
            transition.operation == LanguageServerOperation::Restart
                && transition.keys.contains(key)
        })
    }

    /// Report completed transitions after the engine has synchronized its authorized bindings.
    pub(super) fn complete(&mut self, running: &BTreeMap<Key, Binding>) {
        self.pending.retain(|transition| {
            if transition
                .keys
                .iter()
                .any(|key| self.retiring.contains_key(key))
            {
                return true;
            }
            let result = if transition.operation == LanguageServerOperation::Restart {
                if let Some(error) = transition
                    .keys
                    .iter()
                    .find_map(|key| match running.get(key) {
                        None => Some(
                            "The language server is no longer authorized or available".to_owned(),
                        ),
                        Some(binding) if binding.phase == LanguageServerPhase::Failed => {
                            binding.error.clone()
                        }
                        Some(_) => None,
                    })
                {
                    Err(error)
                } else {
                    Ok(transition.keys.len())
                }
            } else {
                Ok(transition.keys.len())
            };
            let _ = transition.reply.try_send(result);
            false
        });
    }
}

impl Engine {
    /// Resolve the captured `target` and apply the requested native-server `operation`.
    pub(in crate::plugins::session::engine) fn control_language_server(
        &mut self,
        operation: LanguageServerOperation,
        target: LanguageServerTarget,
        reply: async_channel::Sender<Result<usize, String>>,
    ) {
        let keys = match self.language_control_targets(operation, &target) {
            Ok(keys) => keys,
            Err(error) => {
                let _ = reply.try_send(Err(error));
                return;
            }
        };
        if self
            .server_controls
            .pending
            .iter()
            .any(|transition| transition.keys.iter().any(|key| keys.contains(key)))
        {
            let _ = reply.try_send(Err("The language server is already changing state".into()));
            return;
        }
        for key in &keys {
            match operation {
                LanguageServerOperation::Enable | LanguageServerOperation::Restart => {
                    self.server_controls.suspended.remove(key);
                }
                LanguageServerOperation::Stop => {
                    self.server_controls
                        .suspended
                        .insert(key.clone(), Suspension::Stopped(target.document.clone()));
                }
                LanguageServerOperation::Disable => {
                    self.server_controls
                        .suspended
                        .insert(key.clone(), Suspension::Disabled);
                }
            }
            if operation != LanguageServerOperation::Enable
                && let Some(binding) = self.analyzers.remove(key)
            {
                self.server_controls.retire(key.clone(), binding);
            }
        }
        self.server_controls.pending.push(Transition {
            operation,
            keys,
            reply,
        });
        self.sync_analyzers();
    }

    /// Resolve `target` only within the originating Session, even after the UI has switched files.
    fn language_control_targets(
        &self,
        operation: LanguageServerOperation,
        target: &LanguageServerTarget,
    ) -> Result<Vec<Key>, String> {
        let Some(project) = self.projects.get(&target.project) else {
            return Err("The originating Session was closed".into());
        };
        let configs = language_server_configs(
            self.records.values().map(|record| record.as_ref()),
            target.project,
        );
        let configs = if let Some(name) = &target.name {
            vec![select_config(configs, name)?]
        } else {
            let Some((opening, path)) = &target.document else {
                return Err("Open a source file or specify a language server".into());
            };
            configs.into_iter().filter(|config| {
                let attached = self.analyzers.get(&config.key).is_some_and(|binding| {
                    binding.documents.iter().any(|doc| doc.path.as_ref() == Some(path))
                });
                let stopped = self.server_controls.suspended.get(&config.key)
                    .is_some_and(|mode| matches!(mode, Suspension::Stopped(document) if *document == target.document));
                match operation {
                    LanguageServerOperation::Stop | LanguageServerOperation::Restart => attached || stopped,
                    LanguageServerOperation::Enable | LanguageServerOperation::Disable => {
                        attached || stopped || (self.lifecycle.wanted.contains_key(&config.key)
                            && project.document.iter().chain(&project.open_documents).any(|doc| {
                                doc.opening == *opening && doc.path.as_ref() == Some(path)
                            })
                            && path.starts_with(&project.root)
                            && config.module.accepts_filetype(
                                Some(path), self.document_filetype(target.project, *opening),
                            ))
                    }
                }
            }).collect()
        };
        if configs.is_empty() {
            return Err("No language server is associated with the current document".into());
        }
        let mut keys = Vec::with_capacity(configs.len());
        for config in configs {
            let key = config.key;
            if matches!(
                operation,
                LanguageServerOperation::Stop | LanguageServerOperation::Restart
            ) {
                let suspension = self.server_controls.suspended.get(&key);
                if suspension == Some(&Suspension::Disabled) {
                    return Err(
                        "The language server is disabled in this Session. Use :lsp enable.".into(),
                    );
                }
                if !self.analyzers.contains_key(&key)
                    && !self.server_controls.retiring.contains_key(&key)
                    && suspension.is_none()
                {
                    return Err(format!(
                        "{} has no existing instance in this Session",
                        config.server.name()
                    ));
                }
            }
            if operation == LanguageServerOperation::Restart {
                if !self.analyzer_allowed(&key) {
                    return Err(format!(
                        "{} is unavailable or lacks project execution permission",
                        config.server.name()
                    ));
                }
            } else if operation == LanguageServerOperation::Enable && !self.analyzer_authority(&key)
            {
                return Err(format!(
                    "{} is unavailable or lacks project execution permission",
                    config.server.name()
                ));
            }
            keys.push(key);
        }
        Ok(keys)
    }

    /// Retire the instances selected by the plugin manager's existing retry command.
    pub(in crate::plugins::session::engine) fn retire_plugin_servers(&mut self, namespace: &str) {
        self.retire_project_servers(namespace, None);
    }

    /// Reopen only live bindings affected by a manual exclusion change; suspended servers stay stopped.
    pub(in crate::plugins::session::engine) fn retire_project_servers(
        &mut self,
        namespace: &str,
        project: Option<u64>,
    ) {
        let keys: Vec<_> = self
            .analyzers
            .keys()
            .filter(|key| {
                key.namespace == namespace && project.is_none_or(|id| id == key.instance.project)
            })
            .cloned()
            .collect();
        for key in keys {
            let binding = self
                .analyzers
                .remove(&key)
                .expect("selected language server");
            self.server_controls.retire(key, binding);
        }
    }
}

/// Match an exact plugin selector, then a server name only when it identifies one module.
fn select_config<'a>(
    configs: Vec<LanguageServerConfig<'a>>,
    name: &str,
) -> Result<LanguageServerConfig<'a>, String> {
    let exact = configs.iter().any(|config| config.matches_config(name));
    let mut matches = configs.into_iter().filter(|config| {
        if exact {
            config.matches_config(name)
        } else {
            config.server.name() == name
        }
    });
    let config = matches
        .next()
        .ok_or_else(|| format!("Unknown language server: {name}"))?;
    if matches.next().is_some() {
        return Err(format!(
            "Ambiguous language server: {name}. Specify plugin-id/module-id."
        ));
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        package::{InstallStore, Installed},
        query,
        session::{Project, State, View, engine::runtime::Runtime},
        tools::rust_analyzer::Server,
    };
    use anyhow::{Context as _, ensure};
    use nanobug_plugin_sdk::DocumentSnapshot;
    use serde_json::json;
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    /// Build two independent configurations that use the same native executable.
    fn installed(id: &str) -> Installed {
        serde_json::from_value(json!({
            "release": {
                "manifest": {
                    "name": id, "id": id, "version": "0.1.0", "engines": { "nanobug": "*" },
                    "modules": [{ "id": "complete", "language_server": "typescript" }]
                },
                "digest": "", "files": {}, "source": "test"
            },
            "grants": ["read_document", "run_tool"], "previous": null, "enabled": true
        }))
        .unwrap()
    }

    #[test]
    fn language_server_targets_require_unique_names_and_complete_qualified_ids() {
        let packages = [installed("js"), installed("ts")];
        let configs = || language_server_configs(&packages, 7);
        assert!(select_config(configs(), "typescript-language-server").is_err());
        let config = select_config(configs(), "ts/complete").unwrap();
        assert_eq!(config.plugin, "ts");
        assert_eq!(config.key.instance.project, 7);
        let view = View {
            installed: Arc::new(packages.into()),
            ..View::default()
        };
        assert_eq!(
            view.language_server_targets(7).collect::<Vec<_>>(),
            ["js", "js/complete", "ts", "ts/complete"]
        );
    }

    /// A separate scope retains independent tool permission and suspension policy.
    fn project(id: u64, directory: &Path) -> Project {
        Project {
            id,
            root: directory.join(id.to_string()),
            document: None,
            open_documents: Vec::new(),
        }
    }

    #[test]
    fn language_server_disable_is_session_local_and_enable_respects_authority() {
        use LanguageServerOperation::*;
        let directory = tempfile::tempdir().unwrap();
        let mut engine = Engine::new(InstallStore::new(directory.path().join("plugins")), true);
        let package = installed("ts");
        let first_key = language_server_configs([&package], 1).remove(0).key;
        let second_key = language_server_configs([&package], 2).remove(0).key;
        engine
            .records
            .insert(first_key.namespace.clone(), Arc::new(package));
        engine.projects.insert(1, project(1, directory.path()));
        engine.projects.insert(2, project(2, directory.path()));
        let command = |engine: &mut Engine, operation| {
            let (reply, result) = async_channel::bounded(1);
            engine.control_language_server(
                operation,
                LanguageServerTarget {
                    project: 1,
                    document: None,
                    name: Some("ts".into()),
                },
                reply,
            );
            result.try_recv().unwrap()
        };
        assert_eq!(command(&mut engine, Disable), Ok(1));
        assert!(!engine.server_controls.can_attach(&first_key));
        assert!(engine.server_controls.can_attach(&second_key));
        assert!(command(&mut engine, Stop).unwrap_err().contains("disabled"));
        assert!(
            command(&mut engine, Restart)
                .unwrap_err()
                .contains("disabled")
        );
        assert!(
            command(&mut engine, Enable)
                .unwrap_err()
                .contains("permission")
        );
        engine
            .project_execution
            .insert(query::project_key(&engine.projects[&1].root), true);
        assert_eq!(command(&mut engine, Enable), Ok(1));
        assert!(engine.server_controls.can_attach(&first_key));
    }

    #[test]
    fn server_restart_waits_for_retired_worker_before_permitting_replacement() {
        let package = installed("ts");
        let key = language_server_configs([&package], 1).remove(0).key;
        let (allow_exit, wait) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = thread::spawn(move || {
            let _ = wait.recv();
        });
        let mut controls = ServerControls::default();
        let binding = Binding {
            root: PathBuf::new(),
            kind: Server::Typescript,
            automatic_exclusions: Arc::default(),
            server_id: 1,
            generation: 1,
            server: Some(Analyzer::with_test_worker(stop.clone(), worker)),
            phase: LanguageServerPhase::Ready,
            documents: Vec::new(),
            languages: BTreeMap::new(),
            saves: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
            semantics: BTreeMap::new(),
            calls: BTreeMap::new(),
            error: None,
        };
        let (reply, answer) = async_channel::bounded(1);
        controls.pending.push(Transition {
            operation: LanguageServerOperation::Restart,
            keys: vec![key.clone()],
            reply,
        });
        controls.retire(key.clone(), binding);
        assert!(stop.load(Ordering::Acquire));
        assert!(!controls.can_attach(&key));
        controls.complete(&BTreeMap::new());
        assert!(answer.try_recv().is_err());

        allow_exit.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !controls.can_attach(&key) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
            controls.retain(|_| true);
        }
        assert!(controls.restart_requested(&key));
    }

    /// Run the actual host with a fake server on the child's private PATH.
    #[test]
    fn language_server_commands_cycle_real_processes_without_overlap() -> anyhow::Result<()> {
        use std::{env, fs, process::Command};

        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let fixture = root.join("fixture_server.rs");
        fs::write(&fixture, include_str!("control/fixture_server.rs"))?;
        let executable = root.join(format!(
            "typescript-language-server{}",
            env::consts::EXE_SUFFIX
        ));
        let compiled = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition", "2024"])
            .arg(&fixture)
            .arg("-o")
            .arg(&executable)
            .output()?;
        ensure!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );

        let inherited = env::var_os("PATH").unwrap_or_default();
        let paths =
            env::join_paths(std::iter::once(root.clone()).chain(env::split_paths(&inherited)))?;
        let module = module_path!().split_once("::").expect("crate path").1;
        let result = Command::new(env::current_exe()?)
            .arg("--exact")
            .arg(format!("{module}::language_server_process_child"))
            .arg("--ignored")
            .env("NANOBUG_TEST_LSP_CONTROL_ROOT", &root)
            .env("PATH", paths)
            .output()?;
        ensure!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        ensure!(
            String::from_utf8_lossy(&result.stdout).contains("1 passed"),
            "The isolated process regression did not run: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        Ok(())
    }

    /// Process fixture entry point. The enclosing test creates its temporary registry and PATH.
    #[test]
    #[ignore = "executed with an isolated mock server by language_server_commands_cycle_real_processes_without_overlap"]
    fn language_server_process_child() -> anyhow::Result<()> {
        use LanguageServerOperation::*;
        use std::{env, fs};

        let root = PathBuf::from(
            env::var_os("NANOBUG_TEST_LSP_CONTROL_ROOT").context("Missing fixture directory")?,
        );
        let mut engine = Engine::new(InstallStore::new(root.join("plugins")), true);
        let package = installed("ts");
        let config = language_server_configs([&package], 1).remove(0);
        let first = config.key;
        let mut second = first.clone();
        second.instance.project = 2;
        let module = config.module.clone();
        engine
            .records
            .insert(first.namespace.clone(), Arc::new(package));
        engine.lifecycle.runtimes.insert(
            first.namespace.clone(),
            Runtime {
                generation: 1,
                ..Runtime::default()
            },
        );
        for key in [&first, &second] {
            let id = key.instance.project;
            let mut project = project(id, &root);
            fs::create_dir_all(&project.root)?;
            let source = project.root.join("test.ts");
            fs::write(&source, "let disk = 1;\n")?;
            engine
                .project_execution
                .insert(query::project_key(&project.root), true);
            project.document = Some(Arc::new(DocumentSnapshot {
                path: Some(source),
                opening: id,
                revision: 1,
                text: "let unsaved = 2;\n".into(),
            }));
            engine.projects.insert(id, project);
            engine.lifecycle.wanted.insert(key.clone(), module.clone());
            engine.view.modules.insert(key.clone(), State::Ready);
        }
        let test_root = engine.projects[&1].root.clone();
        let source = test_root.join("test.ts");
        let unnamed = LanguageServerTarget {
            project: 1,
            document: Some((1, source.clone())),
            name: None,
        };
        let named = LanguageServerTarget {
            name: Some("ts/complete".into()),
            ..unnamed.clone()
        };

        drive(&mut engine, |host| {
            ready(host, &first)
                && ready(host, &second)
                && test_root.join("last-open.json").is_file()
        })?;
        let peer_id = engine.analyzers[&second].server_id;
        control(&mut engine, Stop, &unnamed)?;
        ensure!(!engine.analyzers.contains_key(&first));
        engine.sync_analyzers();
        ensure!(!engine.analyzers.contains_key(&first));

        control(&mut engine, Restart, &unnamed)?;
        drive(&mut engine, |host| ready(host, &first))?;
        let restored_id = engine.analyzers[&first].server_id;
        control(&mut engine, Restart, &named)?;
        drive(&mut engine, |host| ready(host, &first))?;
        ensure!(engine.analyzers[&first].server_id != restored_id);

        control(&mut engine, Disable, &unnamed)?;
        ensure!(
            control(&mut engine, Restart, &named)
                .unwrap_err()
                .to_string()
                .contains("disabled")
        );
        ensure!(!engine.analyzers.contains_key(&first));
        control(&mut engine, Enable, &unnamed)?;
        drive(&mut engine, |host| ready(host, &first))?;
        ensure!(engine.analyzers[&second].server_id == peer_id);

        let receipt = fs::read_to_string(test_root.join("last-open.json"))?;
        let notification: serde_json::Value = serde_json::from_str(&receipt)?;
        ensure!(notification["params"]["textDocument"]["text"] == "let unsaved = 2;\n");
        ensure!(fs::read_to_string(source)? == "let disk = 1;\n");
        ensure!(
            fs::read_to_string(test_root.join("launches.txt"))?
                .lines()
                .count()
                == 4
        );
        ensure!(!test_root.join("overlapping-server").exists());

        // Settings Apply restarts only this project, adopting the new exclusions in the handshake.
        use crate::plugins::session::{ScanOverride, SettingsWrite};
        let opening = ScanOverride::read(&test_root, "ts")?;
        let changed = ScanOverride::Direct(vec!["generated".into()]);
        for (before, after, excluded) in [
            (opening.clone(), changed.clone(), true),
            (changed, opening, false),
        ] {
            let prior = engine.analyzers[&first].server_id;
            engine.write_settings_page(&SettingsWrite::Scan {
                namespace: first.namespace.clone(),
                digest: String::new(),
                project: 1,
                before,
                after,
            })?;
            drive(&mut engine, |host| {
                ready(host, &first) && host.analyzers[&first].server_id != prior
            })?;
            let initialize = fs::read_to_string(test_root.join("last-initialize.json"))?;
            ensure!(initialize.contains("generated") == excluded);
            ensure!(engine.analyzers[&second].server_id == peer_id);
        }
        ensure!(!test_root.join("overlapping-server").exists());

        control(&mut engine, Stop, &named)?;
        control(
            &mut engine,
            Stop,
            &LanguageServerTarget {
                project: 2,
                document: None,
                name: Some("ts".into()),
            },
        )?;
        Ok(())
    }

    /// Advance the fixture's real server events without launching any WASM plugin runtime.
    fn drive(engine: &mut Engine, mut complete: impl FnMut(&Engine) -> bool) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            while let Ok(event) = engine.events.try_recv() {
                engine.event(event);
            }
            engine.sync_analyzers();
            if complete(engine) {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "Language server fixture timed out: {:?}",
                engine.view.analyzer_errors
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Match the completed initialization handshake for the selected `key`.
    fn ready(engine: &Engine, key: &Key) -> bool {
        engine
            .analyzers
            .get(key)
            .is_some_and(|binding| binding.phase == LanguageServerPhase::Ready)
    }

    /// Wait for an asynchronous host operation, including process teardown.
    fn control(
        engine: &mut Engine,
        operation: LanguageServerOperation,
        target: &LanguageServerTarget,
    ) -> anyhow::Result<usize> {
        let (reply, answer) = async_channel::bounded(1);
        engine.control_language_server(operation, target.clone(), reply);
        let mut result = None;
        drive(engine, |_| {
            result = answer.try_recv().ok();
            result.is_some()
        })?;
        result
            .expect("completed transition")
            .map_err(anyhow::Error::msg)
    }
}
