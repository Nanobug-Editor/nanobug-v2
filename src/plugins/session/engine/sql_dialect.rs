//! Select one language package for generic SQL files in each project.

use super::*;
use std::path::Path;

/// Manifest IDs accepted by the dialect command and recognized for provider selection.
const IDS: &[&str] = &[
    "access-sql",
    "clickhouse",
    "databricks-sql",
    "db2",
    "duckdb",
    "googlesql",
    "h2",
    "mariadb",
    "mysql",
    "oracle-sql",
    "postgresql",
    "snowflake",
    "sqlite",
    "tsql",
];

/// Follow other editor windows' changes without file I/O on each reconciliation.
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);

/// Persist preferences per project and hold each open project's active buffer override.
pub(super) struct Dialects {
    /// Profile-local directory keyed by project root through the existing project key.
    directory: PathBuf,
    /// Preference file associated with each open project scope.
    files: BTreeMap<u64, PathBuf>,
    /// Last readable preference for each open root's file, including inherited defaults.
    preferences: BTreeMap<PathBuf, Option<String>>,
    /// Explicit dialects on the currently selected buffers.
    overrides: BTreeMap<u64, String>,
    /// Earliest time to refresh preferences from other editor windows.
    next_read: Instant,
}

impl Dialects {
    /// Initialize the preference store inside the selected plugin profile.
    pub(super) fn new(state: &Path) -> Self {
        Self {
            directory: state.join("sql-dialects"),
            files: BTreeMap::new(),
            preferences: BTreeMap::new(),
            overrides: BTreeMap::new(),
            next_read: Instant::now(),
        }
    }

    /// Track open project roots and their current SQL buffer overrides.
    pub(super) fn update(
        &mut self,
        projects: &BTreeMap<u64, Project>,
        overrides: BTreeMap<u64, String>,
    ) -> bool {
        self.files.retain(|id, _| projects.contains_key(id));
        for (&id, project) in projects {
            let file = self.files.entry(id).or_insert_with(|| {
                self.directory
                    .join(format!("{}.json", query::project_key(&project.root)))
            });
            self.preferences
                .entry(file.clone())
                .or_insert_with(|| read_dialect(file).ok().flatten());
        }
        self.preferences
            .retain(|file, _| self.files.values().any(|open| open == file));
        let changed = self.overrides != overrides;
        self.overrides = overrides;
        changed
    }

    /// Reload preferences atomically written by other windows, retaining the last readable value.
    pub(super) fn refresh(&mut self) {
        let now = Instant::now();
        if now < self.next_read {
            return;
        }
        self.next_read = now + REFRESH_INTERVAL;
        for (file, preference) in &mut self.preferences {
            if let Ok(value) = read_dialect(file) {
                *preference = value;
            }
        }
    }

    /// Persist a dialect or automatic selection for an open project.
    pub(super) fn set(&mut self, project: u64, dialect: Option<String>) -> anyhow::Result<()> {
        anyhow::ensure!(
            dialect.as_deref().is_none_or(is_sql_dialect),
            "Unknown SQL dialect"
        );
        let file = self.files.get(&project).context("Project was closed")?;
        write_json(file, &dialect)?;
        self.preferences.insert(file.clone(), dialect);
        Ok(())
    }

    /// Resolve the selected buffer's preference before its project's saved dialect.
    fn preferred(&self, project: u64) -> Option<&str> {
        self.overrides
            .get(&project)
            .map(String::as_str)
            .or_else(|| self.project_preference(project))
    }

    /// Return the saved project preference, independent of the current buffer and defaults.
    pub(super) fn project_preference(&self, project: u64) -> Option<&str> {
        self.preferences.get(self.files.get(&project)?)?.as_deref()
    }
}

/// Read one bounded profile preference and validate its stored manifest ID.
fn read_dialect(file: &Path) -> anyhow::Result<Option<String>> {
    let value = read_json::<Option<String>>(file, 1024)?.flatten();
    anyhow::ensure!(
        value.as_deref().is_none_or(is_sql_dialect),
        "Unknown SQL dialect"
    );
    Ok(value)
}

/// Check whether an ID names a SQL dialect handled by the editor.
pub fn is_sql_dialect(id: &str) -> bool {
    IDS.binary_search(&id).is_ok()
}

/// Generic SQL files require selection because the suffix identifies no dialect.
pub fn is_generic_sql(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sql"))
}

/// Apply dialect routing to all SQL feature modules in a known dialect package.
pub(super) fn is_dialect_module(id: &str, module: &Module) -> bool {
    is_sql_dialect(id) && module.document_extensions.contains("sql") && !module.provides.is_empty()
}

impl Engine {
    /// Select an available package namespace, keeping an explicit choice even if its plugin is absent.
    pub(super) fn sql_provider(&self, project: u64) -> Option<&str> {
        let preferred = self.sql_dialects.preferred(project);
        self.records
            .iter()
            .filter(|(namespace, record)| {
                let manifest = &record.release.manifest;
                record.enabled
                    && record.grants.contains(&Permission::ReadDocument)
                    && preferred.is_none_or(|dialect| dialect == manifest.id)
                    && manifest.modules.iter().any(|module| {
                        is_dialect_module(&manifest.id, module)
                            && module.wasm.is_some()
                            && self.config.rule(project, namespace, &module.id)
                                != Some(&config::Rule::Never)
                    })
                    && self.store.compatibility().check(manifest).is_ok()
            })
            .min_by_key(|(namespace, record)| {
                let id = record.release.manifest.id.as_str();
                (id != "postgresql", id, namespace.as_str())
            })
            .map(|(namespace, _)| namespace.as_str())
    }
}
