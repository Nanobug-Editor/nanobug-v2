//! Load and explicitly save the workspace's portable Vim configuration.

use super::Workspace;
use nanobug::i18n;

impl Workspace {
    /// Attach startup Vim defaults to existing project editors and report a load error.
    pub(super) fn load_vim_config(&mut self) {
        if let Some(path) = &self.vim_settings_path
            && let Err(error) = self
                .input_mappings
                .load_config(path)
                .and_then(|()| self.input_mappings.load_highlight_preferences(path))
        {
            self.set_status(i18n::format("vim_config.load_failed", &[("error", &error)]));
        }
        for project in self.projects.iter_mut() {
            project.set_input_mappings(&self.input_mappings);
        }
    }

    /// Persist global configuration in response to `:vimconfig save`.
    pub(super) fn save_vim_config(&mut self) -> bool {
        let (ok, message) = match &self.vim_settings_path {
            Some(path) => match self.input_mappings.save_config(path) {
                Ok(()) => (true, i18n::text("vim_config.saved").into()),
                Err(error) => (
                    false,
                    i18n::format("vim_config.save_failed", &[("error", &error)]),
                ),
            },
            None => (false, i18n::text("vim_config.path_unavailable").into()),
        };
        self.set_status(message);
        ok
    }
}
