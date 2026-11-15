//! Apply the Ex dialect command at project or buffer scope.

use super::{Action, Workspace};
use gpui::{Context, Window};
use nanobug::{
    i18n,
    plugins::session::{is_generic_sql, is_sql_dialect},
};

impl Workspace {
    /// Parse the dialect choice, retain buffer overrides locally, and persist project choices.
    pub(in crate::grid::workspace) fn set_sql_dialect(
        &mut self,
        buffer_only: bool,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialect = match name.as_str() {
            "auto" => None,
            id if is_sql_dialect(id) => Some(name.clone()),
            _ => {
                self.set_status(i18n::text("editor.sql_dialect_usage").into());
                return;
            }
        };
        let Some(project) = self.projects.active_project() else {
            return;
        };
        if buffer_only {
            if !project
                .editor
                .vim()
                .doc
                .path
                .as_deref()
                .is_some_and(is_generic_sql)
            {
                self.set_status(i18n::text("editor.sql_dialect_file").into());
                return;
            }
            self.projects.active_mut().editor.set_sql_dialect(dialect);
            self.sync_plugin_projects();
            self.set_status(i18n::format(
                "editor.sql_dialect_buffer_set",
                &[("name", &name)],
            ));
            return;
        }

        let scope = project.plugin_scope;
        self.sync_plugin_projects();
        let (reply, result) = async_channel::bounded(1);
        if !self.plugins.send(Action::SetProjectSqlDialect {
            project: scope,
            dialect,
            reply,
        }) {
            let message = self.plugins.manager.feedback.clone().unwrap_or_default();
            self.set_status(message);
            return;
        }
        cx.spawn_in(window, async move |workspace, cx| {
            let result = result
                .recv()
                .await
                .unwrap_or_else(|_| Err("Plugin session closed".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let Some(project) = workspace
                        .projects
                        .iter_mut()
                        .find(|project| project.plugin_scope == scope)
                    else {
                        return;
                    };
                    project.status = match result {
                        Ok(()) => {
                            i18n::format("editor.sql_dialect_project_set", &[("name", &name)])
                        }
                        Err(error) => {
                            i18n::format("editor.sql_dialect_save_error", &[("error", &error)])
                        }
                    };
                    workspace.sync(window, cx);
                })
            });
        })
        .detach();
    }
}
