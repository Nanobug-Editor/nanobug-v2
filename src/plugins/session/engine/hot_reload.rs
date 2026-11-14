//! Plugin supervisor hot reload.

use super::*;

/// State retained by the development workflow across supervisor ticks.
pub(super) struct Development {
    /// Current source build and its cancellation flag.
    pub(super) developer_job: Option<(String, Arc<AtomicBool>)>,
    /// Verified build waiting to enter the installation workflow.
    pub(super) developer_ready: Option<Change>,
    /// Development installation retaining its cancellation flag through commit.
    pub(super) developer_install: Option<(String, Arc<AtomicBool>)>,
    /// Last observed source stamps keyed by plugin namespace.
    pub(super) developer_stamps: BTreeMap<String, String>,
    /// Namespaces explicitly queued for a rebuild.
    pub(super) developer_build: BTreeSet<String>,
    /// Last automatically checked namespace, for fair polling order.
    pub(super) developer_last: String,
    /// Earliest time to begin another automatic source check.
    pub(super) developer_due: Instant,
}
impl Default for Development {
    fn default() -> Self {
        Self {
            developer_job: None,
            developer_ready: None,
            developer_install: None,
            developer_stamps: BTreeMap::new(),
            developer_build: BTreeSet::new(),
            developer_last: String::new(),
            developer_due: Instant::now(),
        }
    }
}

impl Engine {
    pub(super) fn cancel_development(&mut self, namespace: &str) {
        for (key, cancelled) in self
            .development
            .developer_job
            .iter()
            .chain(self.development.developer_install.iter())
        {
            if key == namespace {
                cancelled.store(true, Ordering::Release);
            }
        }
        let matches = |change: &Change| {
            matches!(change, Change::Development(..)) && change.namespace() == namespace
        };
        if self
            .development
            .developer_ready
            .as_ref()
            .is_some_and(matches)
        {
            self.development.developer_ready.take();
        }
        if self
            .installation
            .pending_change
            .as_ref()
            .is_some_and(matches)
        {
            self.installation.pending_change.take();
            self.view.installing = false;
        }
        if self
            .installation
            .review
            .as_ref()
            .is_some_and(|review| matches(&review.change))
        {
            self.installation.review.take();
            self.view.review = None;
        }
    }

    pub(super) fn advance_development(&mut self) {
        if !self.view.development
            || !self.enabled
            || self.development.developer_job.is_some()
            || self.installation.pending_change.is_some()
            || self.installation.mutating.is_some()
            || self.installation.inspecting.is_some()
            || self.installation.review.is_some()
        {
            return;
        }
        if let Some(change) = self.development.developer_ready.take() {
            if let Change::Development(package, _) = &change
                && let Some(previous) = self.records.get(&change.namespace())
                && (!package
                    .release
                    .manifest
                    .permissions
                    .is_subset(&previous.grants)
                    || !ApprovedRulePaths::additional(
                        &package.release.manifest.rule_paths,
                        Some(&previous.rule_paths),
                    )
                    .is_empty())
            {
                let token = self.next();
                self.review_package(token, change);
                self.message("Review the new permissions before reloading this plugin.");
                return;
            }
            self.installation.pending_change = Some(change);
            self.view.installing = true;
            self.message("Local build ready. Waiting for plugin operations to finish…");
            return;
        }
        if Instant::now() < self.development.developer_due {
            return;
        }
        let manual = self.development.developer_build.pop_first();
        let force = manual.is_some();
        let namespace = manual.or_else(|| {
            let mut choices = self
                .view
                .developer_settings
                .iter()
                .filter_map(|(key, settings)| {
                    (settings.automatic
                        && settings.source.is_some()
                        && self
                            .releases
                            .get(key)
                            .is_some_and(|release| release.development))
                    .then_some(key)
                })
                .collect::<Vec<_>>();
            if let Some(index) = choices
                .iter()
                .position(|key| key.as_str() == self.development.developer_last.as_str())
            {
                let count = choices.len();
                choices.rotate_left((index + 1) % count);
            }
            choices.first().map(|key| (*key).clone())
        });
        let Some(namespace) = namespace else {
            return;
        };
        self.development.developer_due = Instant::now() + Duration::from_millis(750);
        self.development.developer_last.clone_from(&namespace);
        let Some(release) = self.releases.get(&namespace).cloned() else {
            return;
        };
        let settings = self
            .view
            .developer_settings
            .get(&namespace)
            .cloned()
            .unwrap_or_default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let build = developer::Build {
            settings,
            prior: self.development.developer_stamps.get(&namespace).cloned(),
            force,
            cancelled: Arc::clone(&cancelled),
        };
        let store = self.store.clone();
        let finish = self.finish.clone();
        let key = namespace.clone();
        if self.developer_io.submit(move || {
            let digest = release.digest.clone();
            let result = developer::run(store, release, build);
            let _ = finish.send(ResultEvent::Development(key, digest, result));
        }) {
            self.development.developer_job = Some((namespace, cancelled));
        }
    }
}
