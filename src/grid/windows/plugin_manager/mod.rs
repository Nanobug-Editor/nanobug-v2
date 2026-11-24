//! Plugin management window: browsing state, rows, geometry and character rendering.

pub(in crate::grid) mod comparison;
mod input;
pub(in crate::grid) mod layout;
pub(in crate::grid) mod rows;
mod session;
mod view;

pub(in crate::grid) use input::ListTabClose;
pub(in crate::grid) use layout::{
    SEARCH_BUTTONS, prompt_button_at, secondary_description, secondary_list_entries,
};
use nanobug::{
    i18n,
    plugins::{package::Identity, session::ConfigScope},
};
use serde::{Deserialize, Serialize};
pub(in crate::grid) use session::ManagerSnapshot;
pub(in crate::grid) use view::{ManagerView, Presentation};
use zeroize::Zeroizing;

/// Top-level package source selected in the manager.
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(in crate::grid) enum Page {
    #[default]
    Installed,
    Marketplace,
    Zip,
}

impl Page {
    /// Top-level pages in the order of the tab captions and numeric bindings.
    pub(in crate::grid) const ALL: [Self; 3] = [Self::Installed, Self::Marketplace, Self::Zip];
}

/// Detail tab retained while navigating within a package.
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(in crate::grid) enum Detail {
    #[default]
    Overview,
    Modules,
    Rules,
    Logs,
    Operations,
    Settings,
    Compare,
    Cleanup,
}

/// Footer input and the state needed to commit or cancel its current operation.
pub(in crate::grid) enum Prompt {
    Search {
        /// Query applied before opening the footer.
        original: String,
        /// Selected row restored when cancelling.
        selection: usize,
        /// Scroll offset restored with the selection.
        offset: usize,
    },
    Development,
    DevSource {
        namespace: String,
        settings: nanobug::plugins::session::DeveloperSettings,
    },
    Rule {
        scope: ConfigScope,
        owner: Identity,
        module: String,
    },
    Setting {
        scope: ConfigScope,
        owner: Identity,
        id: String,
        renamed_from: Vec<String>,
        original: Option<serde_json::Value>,
    },
    Credential {
        namespace: String,
        slot: String,
        value: Zeroizing<String>,
    },
    ExportDraft,
    Shortcut {
        namespace: String,
        command: String,
    },
}

/// Plain footer text; credential prompts retain their secret separately in zeroizing storage.
pub(in crate::grid) struct Input {
    /// Selection in the editable single-line search or development path.
    pub(in crate::grid) field: crate::ui::components::grid_text_field::GridTextField,
    pub(in crate::grid) kind: Prompt,
    pub(in crate::grid) value: String,
}

impl Input {
    /// Open a prompt with its caret at the end of the initial value.
    pub(in crate::grid) fn new(kind: Prompt, value: String) -> Self {
        Self {
            kind,
            value,
            field: Default::default(),
        }
    }
}

/// Local navigation and review state, separate from the session's published package data.
#[derive(Default)]
pub(in crate::grid) struct ManagerWindow {
    pub(in crate::grid) review_token: Option<u64>,
    pub(in crate::grid) review_page: usize,
    pub(in crate::grid) page: Page,
    /// Pages closed by title within this instance of the plugin browser.
    closed_list_tabs: [bool; Page::ALL.len()],
    pub(in crate::grid) detail: Option<String>,
    pub(in crate::grid) detail_page: Detail,
    pub(in crate::grid) comparison: Option<comparison::Comparison>,
    pub(in crate::grid) selection: usize,
    pub(in crate::grid) list_selection: usize,
    pub(in crate::grid) offset: usize,
    /// Visual position of the entries between the fixed header and description.
    pub(in crate::grid) list_scroll: nanobug::animation::scroll::GridScroll,
    pub(in crate::grid) search: String,
    pub(in crate::grid) prompt: Option<Input>,
    pub(in crate::grid) help: bool,
    pub(in crate::grid) all_projects: bool,
    pub(in crate::grid) local_project: bool,
    pub(in crate::grid) feedback: Option<String>,
    pub(in crate::grid) confirm_remove: Option<String>,
    pub(in crate::grid) retained_open: bool,
    pub(in crate::grid) clean_credentials: bool,
    pub(in crate::grid) clean_local: bool,
}

impl ManagerWindow {
    /// Resolve the selected configuration scope against the current project identity.
    pub(in crate::grid) fn scope(&self, active: Option<u64>) -> Option<ConfigScope> {
        if self.all_projects {
            Some(ConfigScope::AllProjects)
        } else {
            active.map(|id| {
                if self.local_project && self.detail_page == Detail::Settings {
                    ConfigScope::ProjectLocal(id)
                } else {
                    ConfigScope::Project(id)
                }
            })
        }
    }
    /// Use the comparison's original scope while its preview is open.
    pub(in crate::grid) fn scope_label(&self, active: Option<u64>) -> &'static str {
        match self
            .comparison
            .as_ref()
            .map(|comparison| comparison.scope)
            .or_else(|| self.scope(active))
        {
            Some(ConfigScope::AllProjects) => i18n::text("plugin_manager.all_projects"),
            Some(ConfigScope::ProjectLocal(_)) => {
                i18n::text("plugin_manager.current_project_local")
            }
            _ => i18n::text("plugin_manager.current_project"),
        }
    }
}
