//! Plugin supervisor persistence.

use super::*;

impl Engine {
    pub(super) fn draft_for(&self, key: &Key) -> Option<&String> {
        let project = self.projects.get(&key.instance.project)?;
        self.draft.get(
            &key.namespace,
            &query::project_key(&project.root),
            &key.instance.module,
        )
    }

    pub(super) fn set_draft(&mut self, key: Key, text: String) {
        if text.len() > MAX_DRAFT {
            self.message("Plugin input is too long");
            return;
        }
        let Some(project) = self.projects.get(&key.instance.project) else {
            return;
        };
        if !self.records.get(&key.namespace).is_some_and(|record| {
            record
                .release
                .manifest
                .modules
                .iter()
                .any(|module| module.id == key.instance.module && module.interactive)
        }) {
            return;
        }
        self.draft.record(
            &key.namespace,
            query::project_key(&project.root),
            &key.instance.module,
            text.clone(),
        );
        if let Some(surface) = self.view.surfaces.get_mut(&key) {
            surface.draft = text;
        }
        self.publish_draft_state();
    }

    pub(super) fn draft_scope(&self, scope: Option<u64>) -> anyhow::Result<Option<String>> {
        scope
            .map(|id| {
                self.projects
                    .get(&id)
                    .map(|project| query::project_key(&project.root))
                    .context("Project was closed")
            })
            .transpose()
    }

    pub(super) fn publish_draft_state(&mut self) {
        self.view.draft_pending = self.draft.pending();
        self.changed = true;
    }

    pub(super) fn flush_drafts(&mut self, force: bool, scope: Option<&str>) -> anyhow::Result<()> {
        if !self.draft.pending() || (!force && self.draft_write.elapsed() < DRAFT_INTERVAL) {
            return Ok(());
        }
        self.draft_write = Instant::now();
        let result = self.draft.flush(scope);
        if let Err(error) = &result {
            self.message(format!("Could not save plugin input: {error:#}"));
        }
        self.publish_draft_state();
        result
    }

    pub(super) fn clean_local(&mut self, namespace: &str) -> anyhow::Result<()> {
        self.config.clean_settings(namespace)?;
        self.draft.clean(namespace)?;
        self.publish_draft_state();
        Ok(())
    }
}
