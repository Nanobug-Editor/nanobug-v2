//! Plugin supervisor assets.

use super::*;

impl Engine {
    pub(super) fn open_asset(
        &mut self,
        key: Key,
        generation: u64,
        result: anyhow::Result<PathBuf>,
    ) {
        let id = self.next();
        let input = self.input(&key, PluginEvent::Open, false);
        let document = self.active_document(&key).cloned();
        let settings_revision = self.config.settings_revision(key.instance.project);
        let busy = self.installation.busy(&key.namespace);
        let Some(runtime) = self
            .lifecycle
            .runtimes
            .get_mut(&key.namespace)
            .filter(|runtime| runtime.generation == generation)
        else {
            return;
        };
        let Some(live) = runtime.instances.get_mut(&key.instance) else {
            return;
        };
        live.open_job = false;
        let Some(input) = input.filter(|_| {
            !busy
                && self.lifecycle.wanted.contains_key(&key)
                && !runtime.closing.contains(&key.instance.project)
        }) else {
            return;
        };
        let path = match result {
            Ok(path) => path,
            Err(error) => {
                live.failure = Some(format!("{error:#}"));
                return;
            }
        };
        let Some(worker) = &runtime.worker else {
            return;
        };
        let version = signature(&input, settings_revision);
        let request = Request::Open {
            id,
            instance: key.instance.clone(),
            wasm: path,
            input: Box::new(input),
            interactive: live.module.interactive,
        };
        if worker.post(request).is_ok() {
            live.last = Some(version.clone());
            live.force_document_refresh = false;
            if live.module.interactive
                || live
                    .module
                    .provides
                    .contains(&crate::plugins::package::Provider::Document)
            {
                live.pending_document = document;
            }
            live.open_job = true;
            runtime.calls.insert(
                id,
                Call {
                    instance: key.instance,
                    version: version.0,
                    documents: Vec::new(),
                    format_path: None,
                    deadline: Instant::now() + PREPARE_TIMEOUT,
                    started: Instant::now(),
                    kind: CallKind::Open,
                    reply: None,
                    credential_read: None,
                },
            );
        }
    }
}
