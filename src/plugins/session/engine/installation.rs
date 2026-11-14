//! Plugin supervisor installation.

use super::*;
pub(super) enum Change {
    Install(Box<Prepared>),
    Development(Box<Prepared>, developer::Candidate),
    Remove(String),
}

impl Change {
    pub(super) fn namespace(&self) -> String {
        match self {
            Self::Install(package) | Self::Development(package, _) => {
                package.release.manifest.identity().namespace()
            }
            Self::Remove(key) => key.clone(),
        }
    }

    /// Bind the paths displayed in this review to the exact staged package.
    pub(super) fn approve_rule_paths(&mut self, paths: BTreeSet<String>) {
        match self {
            Self::Install(package) | Self::Development(package, _) => {
                package.reviewed_rule_paths = paths;
            }
            Self::Remove(_) => unreachable!("no permission review for removal"),
        }
    }
}

/// The pending change owns the exact staged package and the external paths shown with it.
pub(super) struct PackageReview {
    pub(super) token: u64,
    pub(super) change: Change,
    pub(super) rule_paths: BTreeSet<String>,
}

/// State retained by the installation workflow across supervisor ticks.
#[derive(Default)]
pub(super) struct Installations {
    /// Installed candidates that remain eligible for startup rollback.
    pub(super) ready_candidates: BTreeSet<String>,
    /// Reviewed change waiting for active plugin operations to finish.
    pub(super) pending_change: Option<Change>,
    /// Namespace whose filesystem installation is currently being committed.
    pub(super) mutating: Option<String>,
    /// Exact staged package and paths awaiting the user decision.
    pub(super) review: Option<PackageReview>,
    /// Token of the archive inspection whose result is still current.
    pub(super) inspecting: Option<u64>,
}

impl Engine {
    pub(super) fn read_catalog(&mut self) {
        let root = self.store.state_root().to_path_buf();
        let finish = self.finish.clone();
        if !self.install.submit(move || {
            let result = (|| {
                let mut entries = read_json::<Vec<CatalogEntry>>(
                    &root.join("marketplace.json"),
                    2 * 1024 * 1024,
                )?
                .unwrap_or_default();
                anyhow::ensure!(entries.len() <= 1000, "Too many marketplace entries");
                for entry in &mut entries {
                    crate::plugins::package::Identity::new(
                        entry.identity.name.clone(),
                        entry.identity.id.clone(),
                    )?;
                    anyhow::ensure!(
                        entry.version.pre.is_empty(),
                        "Marketplace entry is not a stable release"
                    );
                    entry.zip = root.join(&entry.zip);
                }
                entries.sort_by(|a, b| {
                    a.identity
                        .cmp(&b.identity)
                        .then_with(|| b.version.cmp(&a.version))
                });
                Ok(entries)
            })();
            let _ = finish.send(ResultEvent::Catalog(result));
        }) {
            self.message("Marketplace is busy. Reopen Marketplace to read it.");
        }
    }

    /// Inspect an `archive` chosen in the local file browser and save its parent for the next open.
    pub(super) fn inspect_chosen_zip(&mut self, archive: PathBuf) {
        let saved = if let Some(directory) = archive.parent().filter(|path| path.is_absolute()) {
            self.view.local_zip_directory = Some(directory.to_path_buf());
            self.changed = true;
            // Keep the native name intact even for paths that cannot be represented as UTF-8.
            write_json(
                &self.store.state_root().join(LOCAL_ZIP_DIRECTORY),
                &directory.as_os_str(),
            )
        } else {
            Ok(())
        };
        self.inspect(archive, None, false);
        if let Err(error) = saved {
            self.message(format!(
                "Could not remember the local ZIP directory: {error:#}"
            ));
        }
    }

    pub(super) fn inspect(
        &mut self,
        path: PathBuf,
        expected: Option<CatalogEntry>,
        development: bool,
    ) {
        if self.installation.inspecting.is_some()
            || self.installation.pending_change.is_some()
            || self.installation.mutating.is_some()
        {
            return;
        }
        self.installation.review.take();
        self.view.review = None;
        let id = self.next();
        let store = self.store.clone();
        let finish = self.finish.clone();
        let candidates = expected.as_ref().map(|entry| {
            let installed = self
                .records
                .get(&entry.identity.namespace())
                .map(|record| &record.release.manifest.version);
            self.view
                .catalog
                .iter()
                .filter(|candidate| {
                    candidate.identity == entry.identity
                        && installed.is_none_or(|version| candidate.version > *version)
                })
                .take(16)
                .cloned()
                .collect::<Vec<_>>()
        });
        if self.install.submit(move || {
            let result = match candidates {
                None if development => store.prepare_directory(&path),
                None => store.prepare(&path),
                Some(candidates) => prepare_catalog(&store, candidates),
            };
            let _ = finish.send(ResultEvent::Prepared(id, result));
        }) {
            self.installation.inspecting = Some(id);
            self.view.installing = true;
            self.message("Checking ZIP…");
        } else {
            self.message("Installer is busy");
        }
    }

    pub(super) fn review_package(&mut self, token: u64, change: Change) {
        let package = match &change {
            Change::Install(package) | Change::Development(package, _) => package,
            Change::Remove(_) => unreachable!("no permission review for removal"),
        };
        let manifest = &package.release.manifest;
        let previous = self.records.get(&manifest.identity().namespace());
        let mut additional: BTreeSet<_> = previous.map_or_else(
            || manifest.permissions.clone(),
            |old| {
                manifest
                    .permissions
                    .difference(&old.grants)
                    .copied()
                    .collect()
            },
        );
        // A deliberate reinstall of the same package also lets the user reapprove moved paths.
        let rule_paths = if matches!(change, Change::Install(_))
            && previous.is_some_and(|old| old.release.digest == package.release.digest)
        {
            manifest.rule_paths.clone()
        } else {
            ApprovedRulePaths::additional(&manifest.rule_paths, previous.map(|old| &old.rule_paths))
        };
        if !rule_paths.is_empty() {
            additional.insert(Permission::ReadRulePaths);
        }
        self.view.review = Some(InstallReview {
            token,
            manifest: manifest.clone(),
            additional,
            additional_rule_paths: rule_paths.iter().map(PathBuf::from).collect(),
        });
        self.installation.review = Some(PackageReview {
            token,
            change,
            rule_paths,
        });
    }

    pub(super) fn advance_install(&mut self) {
        let Some(change) = &self.installation.pending_change else {
            return;
        };
        let key = change.namespace();
        if self.requests.service_pending.contains_key(&key) {
            return;
        }
        if self
            .lifecycle
            .runtimes
            .get(&key)
            .is_some_and(|runtime| !runtime.calls.is_empty())
        {
            return;
        }
        let change = self
            .installation
            .pending_change
            .take()
            .expect("pending change");
        match change {
            Change::Install(_) | Change::Development(..) => {
                let (package, candidate) = match change {
                    Change::Install(package) => (package, None),
                    Change::Development(package, candidate) => (package, Some(candidate)),
                    Change::Remove(_) => unreachable!("installation candidate"),
                };
                let cancelled = candidate.as_ref().map(|build| Arc::clone(&build.cancelled));
                let store = self.store.clone();
                let finish = self.finish.clone();
                let namespace = key.clone();
                if self.install.submit(move || {
                    let grants = package.release.manifest.permissions.clone();
                    let result = store.install_checked(*package, &grants, |installed| {
                        candidate.map_or(Ok(()), |build| {
                            build.verify(installed)?;
                            if !build.permissions_reviewed {
                                let prior =
                                    installed.context("Development plugin is not installed")?;
                                anyhow::ensure!(
                                    grants.is_subset(&prior.grants),
                                    "Review the new development permissions before installing"
                                );
                            }
                            Ok(())
                        })
                    });
                    let _ = finish.send(ResultEvent::Installed(namespace, result));
                }) {
                    self.development.developer_install =
                        cancelled.map(|cancelled| (key.clone(), cancelled));
                    self.installation.mutating = Some(key);
                    self.message("Preparing and installing plugin…");
                } else {
                    self.view.installing = false;
                    self.message("Installer is busy. Select the ZIP again.");
                }
            }
            Change::Remove(namespace) => {
                self.view.installing = false;
                self.lifecycle.runtimes.remove(&namespace);
                self.view
                    .surfaces
                    .retain(|key, _| key.namespace != namespace);
                let result = self.store.uninstall(&namespace);
                self.reload();
                match result {
                    Ok(()) => self.message("Plugin uninstalled. Local data kept."),
                    Err(error) => self.message(format!("{error:#}")),
                }
            }
        }
    }

    pub(super) fn recover_candidate(&mut self, namespace: &str) -> bool {
        if !self.installation.ready_candidates.contains(namespace) {
            return false;
        }
        let Some(release) = self.releases.get(namespace) else {
            return false;
        };
        match self.store.rollback(namespace, &release.digest) {
            Ok(()) => {
                self.lifecycle.runtimes.remove(namespace);
                self.view
                    .surfaces
                    .retain(|key, _| key.namespace != namespace);
                self.reload();
                self.message("Plugin could not start. Previous version restored.");
                true
            }
            Err(error) => {
                self.message(format!(
                    "Plugin could not start. Could not restore previous version: {error:#}"
                ));
                false
            }
        }
    }
}

pub(super) fn prepare_catalog(
    store: &InstallStore,
    candidates: Vec<CatalogEntry>,
) -> anyhow::Result<Prepared> {
    let mut first_error = None;
    for entry in candidates {
        match store.prepare(&entry.zip).and_then(|package| {
            let manifest = &package.release.manifest;
            anyhow::ensure!(
                manifest.identity() == entry.identity && manifest.version == entry.version,
                "Marketplace entry does not match the ZIP"
            );
            Ok(package)
        }) {
            Ok(package) => return Ok(package),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| anyhow::anyhow!("No newer marketplace release is available")))
}

impl Installations {
    /// A package remains busy while its staged change is pending or being committed.
    pub(super) fn busy(&self, namespace: &str) -> bool {
        self.mutating.as_deref() == Some(namespace)
            || self
                .pending_change
                .as_ref()
                .is_some_and(|change| change.namespace() == namespace)
    }

    /// Move the exact reviewed package into the pending slot only for its current `token`.
    pub(super) fn approve(&mut self, token: u64) -> bool {
        if !self
            .review
            .as_ref()
            .is_some_and(|review| review.token == token)
        {
            return false;
        }
        let PackageReview {
            mut change,
            rule_paths,
            ..
        } = self.review.take().expect("approved package");
        change.approve_rule_paths(rule_paths);
        if let Change::Development(_, candidate) = &mut change {
            candidate.permissions_reviewed = true;
        }
        self.pending_change = Some(change);
        true
    }

    /// Cancel staged work only before an installation has entered its filesystem commit.
    pub(super) fn cancel(&mut self) -> bool {
        if self.mutating.is_some() {
            return false;
        }
        self.inspecting.take();
        self.review.take();
        self.pending_change.take();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cancellation must not discard the state needed to finish an in-flight filesystem change.
    #[test]
    fn cancellation_preserves_in_flight_changes_and_releases_staged_work() {
        let mut state = Installations {
            pending_change: Some(Change::Remove("sample".into())),
            mutating: Some("sample".into()),
            inspecting: Some(7),
            ..Default::default()
        };
        assert!(state.busy("sample"));
        assert!(!state.busy("unrelated"));
        assert!(!state.cancel());
        assert!(state.pending_change.is_some());
        assert_eq!(state.inspecting, Some(7));
        state.mutating = None;
        assert!(state.cancel());
        assert!(!state.busy("sample"));
        assert!(state.inspecting.is_none());
        assert!(!state.approve(7));
    }
}
