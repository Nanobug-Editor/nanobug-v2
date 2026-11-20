//! Commit native settings through the supervisor that owns package and runtime state.

use super::Engine;
use crate::plugins::session::{
    ConfigScope, ScanOverride, SettingsCatalog, SettingsWrite, declared_server,
};
use anyhow::{Context, Result, ensure};

impl Engine {
    /// Read project TOML on the supervisor, including the legacy Rust fallback.
    pub(super) fn settings_catalog(&self, project: Option<u64>) -> Result<SettingsCatalog> {
        let mut scans = std::collections::BTreeMap::new();
        if let Some(id) = project {
            let project = self.projects.get(&id).context("Project is closed")?;
            for (namespace, installed) in &self.records {
                let manifest = &installed.release.manifest;
                if manifest
                    .modules
                    .iter()
                    .any(|m| declared_server(m).is_some())
                {
                    scans.insert(
                        namespace.clone(),
                        ScanOverride::read(&project.root, &manifest.id)
                            .map_err(|error| format!("{error:#}")),
                    );
                }
            }
        }
        Ok(SettingsCatalog {
            installed: self.view.installed.clone(),
            scopes: self.config.committed_settings(),
            scans,
        })
    }

    pub(super) fn write_settings_page(&mut self, write: &SettingsWrite) -> Result<()> {
        let (namespace, digest) = match write {
            SettingsWrite::Enabled {
                namespace, digest, ..
            }
            | SettingsWrite::Manifest {
                namespace, digest, ..
            }
            | SettingsWrite::Scan {
                namespace, digest, ..
            } => (namespace, digest),
        };
        let installed = self
            .records
            .get(namespace)
            .context("Plugin is no longer installed")?;
        ensure!(
            &installed.release.digest == digest,
            "Plugin has been updated. Reopen settings."
        );
        let manifest = &installed.release.manifest;
        match write {
            SettingsWrite::Enabled {
                namespace,
                digest,
                before,
                after,
            } => {
                ensure!(
                    self.installation.mutating.is_none()
                        && self.installation.pending_change.is_none(),
                    "An installation is in progress"
                );
                self.store
                    .change_enabled(namespace, Some((digest, *before)), *after)?;
                self.reload();
            }
            SettingsWrite::Manifest {
                namespace,
                scope,
                fields,
                owner_was_absent,
                ..
            } => {
                // Runtime writes already authorized against the old settings must finish first.
                let owns = |key: &super::Key| {
                    &key.namespace == namespace
                        && match scope {
                            ConfigScope::AllProjects => true,
                            ConfigScope::Project(id) | ConfigScope::ProjectLocal(id) => {
                                key.instance.project == *id
                            }
                        }
                };
                ensure!(
                    !self
                        .requests
                        .file_writes
                        .iter()
                        .any(|write| owns(&write.owner))
                        && !self.tool_writes.keys().any(owns),
                    "Plugin is finishing a write. Retry Apply."
                );
                self.config
                    .patch_settings(*scope, manifest, *owner_was_absent, fields)?;
            }
            SettingsWrite::Scan {
                namespace,
                project,
                before,
                after,
                ..
            } => {
                ensure!(
                    manifest
                        .modules
                        .iter()
                        .any(|module| declared_server(module).is_some()),
                    "Plugin has no language server"
                );
                let root = &self
                    .projects
                    .get(project)
                    .context("Project is closed")?
                    .root;
                before.replace(root, &manifest.id, after)?;
                self.retire_project_servers(namespace, Some(*project));
            }
        }
        Ok(())
    }
}
