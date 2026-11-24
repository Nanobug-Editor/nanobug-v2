//! Apply :Cfilter and :Lfilter to the current snapshot, preserving matching source anchors.

use super::Workspace;
use gpui::{Context, Window};
use nanobug::vim::ListFilter;

impl Workspace {
    /// Match each entry's message and displayed file name before pushing the new list.
    pub(in crate::grid::workspace) fn apply_list_filter(
        &mut self,
        location: bool,
        filter: ListFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.active_project() else {
            return;
        };
        let root = project.root.clone();
        self.filter_result_list(
            location,
            filter.title(location),
            |entry| {
                let Some(path) = entry.path.as_deref() else {
                    return filter.keeps(&entry.message, &[""]);
                };
                let name = path.strip_prefix(&root).unwrap_or(path).to_string_lossy();
                if cfg!(windows) && name.contains('\\') {
                    // Accept forward slashes as well as the separator printed in Windows lists.
                    filter.keeps(&entry.message, &[&name, &name.replace('\\', "/")])
                } else {
                    filter.keeps(&entry.message, &[&name])
                }
            },
            window,
            cx,
        );
    }
}
