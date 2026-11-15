//! Read-only host data for rules. Every filesystem observation retains its dependency.

use super::package::{ApprovedRuleFile, ApprovedRulePaths, DirectoryId, hash_bytes};
use super::safe_path::PinnedPath;
use anyhow::{Context, Result, ensure};
use nanobug_plugin_sdk::{DocumentSnapshot, Permission, Query, document_wire::MAX_DOCUMENT_BYTES};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

/// A filesystem version used for invalidation, including the absence of a file.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    length: u64,
    directory: bool,
    regular: bool,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl From<fs::Metadata> for Stamp {
    fn from(metadata: fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            created: metadata.created().ok(),
            modified: metadata.modified().ok(),
            length: metadata.len(),
            directory: metadata.is_dir(),
            regular: metadata.is_file(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

/// Record the checked file or its absence during both reads and polling.
fn stamp(target: Option<&PinnedPath>) -> Result<Option<Stamp>, io::Error> {
    let Some(target) = target else {
        return Ok(None);
    };
    match target.metadata() {
        Ok(metadata) => Ok(Some(metadata.into())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Exact data source observed while evaluating a rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Dependency {
    File(FileDependency),
    Document(Option<String>, Option<(u64, u64)>),
    DocumentPath(Option<PathBuf>),
    /// The observed filename extension, with ASCII letters normalized to lowercase.
    DocumentExtension(Option<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FileDependency {
    root: PathBuf,
    requested: String,
    approved: Option<ApprovedRuleFile>,
    observed: FileObservation,
    failed_open: Option<(Query, IoFailure)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FileObservation {
    PathUnavailable(IoFailure),
    Resolved {
        path: PathBuf,
        before: Result<Option<Stamp>, IoFailure>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IoFailure(io::ErrorKind, Option<i32>);

impl From<&io::Error> for IoFailure {
    fn from(error: &io::Error) -> Self {
        Self(error.kind(), error.raw_os_error())
    }
}

impl FileDependency {
    fn current(&self) -> bool {
        match (
            rule_path(&self.root, &self.requested, self.approved.as_ref()),
            &self.observed,
        ) {
            (Ok(resolved), FileObservation::Resolved { path, before }) if &resolved == path => {
                let Ok(target) = pin_rule_path(path, self.approved.as_ref()) else {
                    return false;
                };
                let current = stamp(target.as_ref()).map_err(|error| IoFailure::from(&error));
                if &current != before {
                    return false;
                }
                self.failed_open.as_ref().is_none_or(|(query, prior)| {
                    let opened = target
                        .as_ref()
                        .ok_or(io::ErrorKind::NotFound.into())
                        .and_then(|target| match query {
                            Query::List => target.read_dir().map(drop),
                            _ => target.open().map(drop),
                        });
                    opened.is_err_and(|error| IoFailure::from(&error) == *prior)
                })
            }
            (Err(error), FileObservation::PathUnavailable(prior)) => error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| IoFailure::from(error) == *prior),
            (Ok(resolved), FileObservation::PathUnavailable(prior)) => {
                pin_rule_path(&resolved, self.approved.as_ref())
                    .is_err_and(|error| IoFailure::from(&error) == *prior)
            }
            _ => false,
        }
    }
}

/// Input and authority for a particular query. Neither is set by the guest.
#[derive(Clone)]
pub(super) struct ContextData {
    pub root: PathBuf,
    pub grants: BTreeSet<Permission>,
    pub rule_paths: ApprovedRulePaths,
    pub document: Option<Arc<DocumentSnapshot>>,
    pub open_documents: Vec<Arc<DocumentSnapshot>>,
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
}

/// Resolve a path below the project root without trusting textual prefixes or symlinks.
pub fn project_path(root: &Path, input: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let supplied = Path::new(input);
    let path = if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        let mut path = root.clone();
        for item in supplied.components() {
            match item {
                Component::Normal(segment) => path.push(segment),
                Component::CurDir => {}
                Component::ParentDir if path != root => {
                    path.pop();
                }
                _ => anyhow::bail!("Path is outside the project"),
            }
        }
        path
    };
    let resolved = resolve_prefix(&path)?;
    ensure!(resolved.starts_with(&root), "Path is outside the project");
    Ok(resolved)
}

#[derive(Debug)]
struct RuleFileChanged;

impl std::fmt::Display for RuleFileChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("Approved external rule path no longer names the original file location")
    }
}

impl std::error::Error for RuleFileChanged {}

/// Map ordinary and verbatim drive and UNC names to the same lexical spelling.
fn normalized_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let mut parts = path.components();
        if let Some(Component::Prefix(prefix)) = parts.next() {
            let mut normal = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
                Prefix::VerbatimUNC(host, share) => {
                    let mut normal = std::ffi::OsString::from("\\\\");
                    normal.push(host);
                    normal.push("\\");
                    normal.push(share);
                    normal.push("\\");
                    PathBuf::from(normal)
                }
                _ => return path.components().collect(),
            };
            normal.extend(parts.filter(|part| *part != Component::RootDir));
            return normal;
        }
    }
    path.components().collect()
}

/// Reject scope escapes before querying the disk or contacting a share.
fn rule_path(root: &Path, input: &str, approved: Option<&ApprovedRuleFile>) -> Result<PathBuf> {
    if let Some(approved) = approved {
        return Ok(approved.path.clone());
    }
    let input = Path::new(input);
    let supplied = normalized_path(input);
    let lexical_root = normalized_path(&std::path::absolute(root)?);
    ensure!(
        !input.components().any(|part| part == Component::ParentDir),
        "Path is outside the project"
    );
    let relative = if input.is_absolute() {
        supplied
            .strip_prefix(&lexical_root)
            .context("Path is outside the project")?
    } else {
        &supplied
    };
    ensure!(
        relative.components().all(|part| match part {
            Component::CurDir => true,
            Component::Normal(name) => {
                #[cfg(windows)]
                {
                    name.to_str()
                        .is_some_and(super::package::windows_rule_component)
                }
                #[cfg(not(windows))]
                {
                    let _ = name;
                    true
                }
            }
            _ => false,
        }),
        "Path is outside the project"
    );
    // Keep the opened project name and let the no-follow walker check every component.
    if relative.as_os_str().is_empty() {
        Ok(lexical_root)
    } else {
        Ok(lexical_root.join(relative))
    }
}

/// Hold the resolved parent while checking its installation-time identity.
fn pin_rule_path(
    path: &Path,
    approved: Option<&ApprovedRuleFile>,
) -> io::Result<Option<PinnedPath>> {
    let target = match PinnedPath::new(path) {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if let Some(approved) = approved
        && DirectoryId::from_file(&target.parent()?)? != approved.parent
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            RuleFileChanged,
        ));
    }
    Ok(Some(target))
}

/// Resolve aliases in the existing prefix, including paths not created yet.
pub(super) fn resolve_prefix(path: &Path) -> Result<PathBuf> {
    let mut path = std::path::absolute(path)?;
    let mut missing = Vec::new();
    loop {
        match path.canonicalize() {
            Ok(mut resolved) => {
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(
                    path.file_name()
                        .context("Invalid project path")?
                        .to_os_string(),
                );
                ensure!(path.pop(), "Invalid project path");
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Recheck the original observations without re-reading file contents.
pub(super) fn files_current(deps: &[Dependency]) -> bool {
    deps.iter().all(|dep| match dep {
        Dependency::File(observation) => observation.current(),
        Dependency::Document(..)
        | Dependency::DocumentPath(_)
        | Dependency::DocumentExtension(_) => true,
    })
}

/// Determine if a document dependency has been invalidated by editing or focus changes.
pub(super) fn document_current(
    deps: &[Dependency],
    project: Option<&super::session::Project>,
) -> bool {
    deps.iter().all(|dep| match dep {
        Dependency::Document(path, before) => {
            let current = project
                .and_then(|project| {
                    find_document(
                        &project.root,
                        project.document.as_deref(),
                        &project.open_documents,
                        path.as_deref(),
                    )
                })
                .map(|doc| (doc.opening, doc.revision));
            *before == current
        }
        Dependency::DocumentPath(before) => {
            *before == project.and_then(|project| project.document.as_ref()?.path.clone())
        }
        Dependency::DocumentExtension(before) => {
            *before == document_extension(project.and_then(|project| project.document.as_deref()))
        }
        _ => true,
    })
}

fn find_document<'a>(
    root: &Path,
    active: Option<&'a DocumentSnapshot>,
    open: &'a [Arc<DocumentSnapshot>],
    path: Option<&str>,
) -> Option<&'a DocumentSnapshot> {
    let Some(path) = path else {
        return active;
    };
    active
        .into_iter()
        .chain(open.iter().map(Arc::as_ref))
        .find(|doc| {
            doc.path.as_deref().is_some_and(|actual| {
                actual == Path::new(path) || actual.strip_prefix(root).ok() == Some(Path::new(path))
            })
        })
}

/// Read named files from the document directory through the authorized project root.
pub(super) fn read_ancestors(
    context: &ContextData,
    names: Vec<String>,
) -> Result<nanobug_plugin_sdk::language::ProjectAncestors> {
    ensure!(
        context.grants.contains(&Permission::ReadProject),
        "Plugin has no project-read grant"
    );
    ensure!(
        !names.is_empty()
            && names.len() <= 4
            && names.iter().all(|name| {
                !name.is_empty()
                    && name.len() <= 128
                    && name != "."
                    && name != ".."
                    && !name.contains(['/', '\\', ':'])
                    && !name.chars().any(char::is_control)
            }),
        "Expected up to four project file basenames"
    );
    let root = normalized_path(&context.root);
    let path = context
        .document
        .as_ref()
        .and_then(|doc| doc.path.as_ref())
        .map(|path| normalized_path(path));
    let relative = path.as_ref().and_then(|path| path.strip_prefix(&root).ok());
    let directory = relative
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(""));
    let mut remaining = 128 * 1024;
    let mut files = Vec::new();
    let mut directories = directory.ancestors().take(32).collect::<Vec<_>>();
    if directories
        .last()
        .is_some_and(|dir| !dir.as_os_str().is_empty())
    {
        directories.push(Path::new(""));
    }
    for directory in directories {
        for name in &names {
            ensure!(
                Instant::now() < context.deadline && !context.cancelled.load(Ordering::Acquire),
                "Project discovery expired or the project closed"
            );
            let path = slash_path(&directory.join(name));
            if remaining == 0 {
                continue;
            }
            let (result, _) = run(
                context,
                Query::ReadFile,
                &json!({"path": path, "limit": remaining}),
            );
            if let Ok(value) = result
                && value["complete"] == true
                && let Some(text) = value["text"].as_str()
            {
                remaining -= text.len();
                files.push(nanobug_plugin_sdk::language::AncestorFile {
                    path,
                    text: text.strip_prefix('\u{feff}').unwrap_or(text).to_owned(),
                });
            }
        }
    }
    Ok(nanobug_plugin_sdk::language::ProjectAncestors {
        document: relative.map(slash_path),
        files,
    })
}

/// Encode a host path relative to the project for platform-neutral WASM consumers.
fn slash_path(path: &Path) -> String {
    path.iter()
        .map(|component| component.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Evaluate one rule query with bounded output, retrying only transient I/O errors.
pub(super) fn run(
    context: &ContextData,
    kind: Query,
    args: &Value,
) -> (Result<Value, String>, Option<Dependency>) {
    run_retries(context, kind, args, std::thread::sleep)
}

fn run_retries(
    context: &ContextData,
    kind: Query,
    args: &Value,
    mut sleep: impl FnMut(Duration),
) -> (Result<Value, String>, Option<Dependency>) {
    let mut observed = None;
    for (attempt, delay) in [0, 100, 300].into_iter().enumerate() {
        if delay > 0 {
            let wait = Duration::from_millis(delay);
            if context.deadline.saturating_duration_since(Instant::now()) < wait {
                break;
            }
            sleep(wait);
        }
        let result = query(context, kind, args, &mut observed);
        match result {
            Ok(value) => return (Ok(value), observed),
            Err(error)
                if attempt < 2 && error.downcast_ref::<io::Error>().is_some_and(transient) => {}
            Err(error) => return (Err(format!("{error:#}")), observed),
        }
    }
    (Err("Rule data deadline expired".into()), observed)
}

fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) || cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

/// Return the document's extension as a normalized, independently trackable lookup.
fn document_extension(document: Option<&DocumentSnapshot>) -> Option<String> {
    let path = document?.path.as_deref()?;
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

fn query(
    context: &ContextData,
    kind: Query,
    args: &Value,
    dependency: &mut Option<Dependency>,
) -> Result<Value> {
    ensure!(
        Instant::now() < context.deadline,
        "Rule data deadline expired"
    );
    ensure!(
        !context.cancelled.load(Ordering::Acquire),
        "Rule evaluation cancelled"
    );
    if matches!(kind, Query::DocumentPath | Query::DocumentExtension) {
        ensure!(
            context.grants.contains(&Permission::ReadDocument),
            "Plugin has no document-read grant"
        );
        if kind == Query::DocumentExtension {
            let extension = document_extension(context.document.as_deref());
            let value = json!(extension);
            *dependency = Some(Dependency::DocumentExtension(extension));
            return Ok(value);
        }
        let path = context.document.as_ref().and_then(|doc| doc.path.clone());
        let value = serde_json::to_value(&path)?;
        *dependency = Some(Dependency::DocumentPath(path));
        return Ok(value);
    }
    if kind == Query::Document {
        ensure!(
            context.grants.contains(&Permission::ReadDocument),
            "Plugin has no document-read grant"
        );
        let path = match args.get("path") {
            None | Some(Value::Null) => None,
            Some(path) => Some(path.as_str().context("Expected a document path")?),
        };
        let document = find_document(
            &context.root,
            context.document.as_deref(),
            &context.open_documents,
            path,
        );
        *dependency = Some(Dependency::Document(
            path.map(str::to_owned),
            document.map(|doc| (doc.opening, doc.revision)),
        ));
        ensure!(
            document.is_none_or(|doc| doc.text.len() <= MAX_DOCUMENT_BYTES),
            "Document exceeds the rule snapshot size limit"
        );
        return Ok(document.map_or(Value::Null, |doc| json!(doc)));
    }
    let requested = args["path"].as_str().context("Expected a path")?;
    let approved = context
        .grants
        .contains(&Permission::ReadRulePaths)
        .then(|| context.rule_paths.allows(requested))
        .flatten();
    ensure!(
        approved.is_some() || context.grants.contains(&Permission::ReadProject),
        "Plugin has no project-read grant or approval for this external rule file"
    );
    ensure!(
        approved.is_none() || kind != Query::List,
        "External rule approval only covers the named file"
    );
    let observation = |observed| FileDependency {
        root: context.root.clone(),
        requested: requested.into(),
        approved: approved.cloned(),
        observed,
        failed_open: None,
    };
    let path = match rule_path(&context.root, requested, approved) {
        Ok(path) => path,
        Err(error) => {
            if let Some(cause) = error.downcast_ref::<io::Error>() {
                *dependency = Some(Dependency::File(observation(
                    FileObservation::PathUnavailable(cause.into()),
                )));
            }
            return Err(error);
        }
    };
    let target = match pin_rule_path(&path, approved) {
        Ok(target) => target,
        Err(error) => {
            *dependency = Some(Dependency::File(observation(
                FileObservation::PathUnavailable((&error).into()),
            )));
            return Err(error.into());
        }
    };
    let observed = stamp(target.as_ref());
    *dependency = Some(Dependency::File(observation(FileObservation::Resolved {
        path: path.clone(),
        before: observed.as_ref().cloned().map_err(IoFailure::from),
    })));
    let observed = observed?;
    if kind == Query::Exists {
        return Ok(json!(observed.is_some()));
    }
    let Some(info) = observed else {
        return Ok(Value::Null);
    };
    let target = target.context("Rule file has no checked path")?;
    let record_open_failure = |error: &io::Error| {
        if let Some(Dependency::File(observed)) = dependency {
            observed.failed_open = Some((kind, error.into()));
        }
    };
    if kind == Query::List {
        ensure!(info.directory, "Expected a directory");
        let mut names = Vec::new();
        let mut complete = true;
        let mut size = 0;
        for item in target.read_dir().inspect_err(record_open_failure)? {
            if Instant::now() >= context.deadline || names.len() >= 1024 {
                complete = false;
                break;
            }
            ensure!(
                !context.cancelled.load(Ordering::Acquire),
                "Rule evaluation cancelled"
            );
            let name = item?.to_string_lossy().into_owned();
            size += serde_json::to_vec(&name)?.len() + 1;
            if size > 256 * 1024 {
                complete = false;
                break;
            }
            names.push(name);
        }
        names.sort();
        return Ok(json!({ "entries": names, "complete": complete }));
    }
    ensure!(info.regular, "Expected a regular file");
    let mut file = target.open().inspect_err(record_open_failure)?;
    if Stamp::from(file.metadata()?) != info {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "File changed while opening it").into(),
        );
    }
    let offset = args["offset"].as_u64().unwrap_or(0);
    file.seek(SeekFrom::Start(offset))?;
    if kind == Query::ScanText {
        let needle = args["contains"]
            .as_str()
            .context("Expected text to search")?;
        ensure!(
            !needle.is_empty() && needle.len() <= 4096,
            "Invalid search text length"
        );
        let limit = args["limit"].as_u64().unwrap_or(32).clamp(1, 256) as usize;
        return scan(file, needle, offset, limit, context);
    }
    let maximum = if kind == Query::ReadBytes {
        64 * 1024
    } else {
        128 * 1024
    };
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(32 * 1024)
        .clamp(1, maximum);
    let mut bytes = Vec::new();
    let lookahead = if kind == Query::ReadFile { 3 } else { 0 };
    file.take(limit + lookahead).read_to_end(&mut bytes)?;
    ensure!(
        Instant::now() < context.deadline,
        "Rule data deadline expired"
    );
    ensure!(
        !context.cancelled.load(Ordering::Acquire),
        "Rule evaluation cancelled"
    );
    if kind == Query::ReadBytes {
        let next = offset.saturating_add(bytes.len() as u64);
        let complete = next >= info.length;
        return Ok(json!({ "bytes": bytes, "next_offset": next, "complete": complete }));
    }
    let selected = &bytes[..bytes.len().min(limit as usize)];
    ensure!(!selected.contains(&0), "Binary data requires readBytes");
    let text = match std::str::from_utf8(selected) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() && selected.len() < bytes.len() => {
            let valid = error.valid_up_to();
            if valid != 0 {
                std::str::from_utf8(&bytes[..valid])?
            } else {
                // A limit smaller than the first scalar still advances to a valid boundary.
                let width = match bytes[0] {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                std::str::from_utf8(
                    bytes
                        .get(..width)
                        .context("Text ended inside a UTF-8 character")?,
                )?
            }
        }
        Err(error) => return Err(error).context("Text is not UTF-8 at the requested offset"),
    };
    let next = offset.saturating_add(text.len() as u64);
    Ok(json!({ "text": text, "next_offset": next, "complete": next >= info.length }))
}

/// Scan bounded chunks without allocating the source file or losing cross-chunk matches.
fn scan(
    mut file: File,
    needle: &str,
    offset: u64,
    limit: usize,
    context: &ContextData,
) -> Result<Value> {
    const CHUNK: usize = 32 * 1024;
    let mut matches = Vec::new();
    let mut buffer = vec![0; CHUNK + needle.len() + 3];
    let mut start = offset;
    let mut carry = 0;
    loop {
        ensure!(
            !context.cancelled.load(Ordering::Acquire),
            "Rule evaluation cancelled"
        );
        // Leave time for the host and the JS worker to receive an incomplete result.
        if context.deadline.saturating_duration_since(Instant::now()) <= Duration::from_millis(40) {
            return Ok(json!({ "offsets": matches, "next_offset": start, "complete": false }));
        }
        let read = file.read(&mut buffer[carry..carry + CHUNK])?;
        if Instant::now() >= context.deadline {
            return Ok(json!({ "offsets": matches, "next_offset": start, "complete": false }));
        }
        let len = carry + read;
        ensure!(
            !buffer[..len].contains(&0),
            "Binary data requires readBytes"
        );
        let text = match std::str::from_utf8(&buffer[..len]) {
            Ok(text) => text,
            Err(error) if read > 0 && error.error_len().is_none() => {
                std::str::from_utf8(&buffer[..error.valid_up_to()])?
            }
            Err(error) => return Err(error).context("Search data is not UTF-8"),
        };
        let mut examined = if read == 0 {
            text.len()
        } else {
            text.len()
                .checked_sub(needle.len())
                .map_or(0, |offset| offset + 1)
        };
        while !text.is_char_boundary(examined) {
            examined += 1;
        }
        for (index, ch) in text[..examined].char_indices() {
            if text[index..].starts_with(needle) {
                matches.push(start + index as u64);
                if matches.len() == limit {
                    return Ok(
                        json!({ "offsets": matches, "next_offset": start + (index + ch.len_utf8()) as u64, "complete": false }),
                    );
                }
            }
        }
        if read == 0 {
            return Ok(
                json!({ "offsets": matches, "next_offset": start + len as u64, "complete": true }),
            );
        }
        carry = len - examined;
        buffer.copy_within(examined..len, 0);
        start += examined as u64;
    }
}

/// Namespace used for local state tied to an opened root.
pub(super) fn project_key(root: &Path) -> String {
    hash_bytes(root.as_os_str().as_encoded_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(windows)]
    #[test]
    fn denied_rule_paths_are_rejected_before_opening_the_root_or_share() {
        // These paths are only passed to the lexical rejection path. No share is contacted.
        let root = Path::new(r"C:\nonexistent-nanobug-fixture\project");
        for input in [
            r"\\localhost\nanobug-unconfigured-fixture\file.txt",
            r"\\?\UNC\localhost\nanobug-unconfigured-fixture\file.txt",
            r"C:\nonexistent-nanobug-fixture\outside.txt",
            r"C:\nonexistent-nanobug-fixture\project\..\outside.txt",
            r"..\outside.txt",
            r"C:drive-relative.txt",
            r"folder\file.txt:stream",
        ] {
            assert_eq!(
                rule_path(root, input, None).unwrap_err().to_string(),
                "Path is outside the project"
            );
        }
    }

    #[test]
    fn document_snapshot_limit_is_an_error_with_the_observed_revision() -> Result<()> {
        let project = tempfile::tempdir()?;
        let context = ContextData {
            root: project.path().to_path_buf(),
            grants: [Permission::ReadDocument].into(),
            rule_paths: ApprovedRulePaths::default(),
            document: Some(Arc::new(DocumentSnapshot {
                path: None,
                opening: 7,
                revision: 3,
                text: "\u{1}".repeat(MAX_DOCUMENT_BYTES + 1),
            })),
            open_documents: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let (result, dependency) = run(&context, Query::Document, &json!({}));
        assert!(result.unwrap_err().contains("snapshot size limit"));
        assert_eq!(dependency, Some(Dependency::Document(None, Some((7, 3)))));
        let (result, _) = run(&context, Query::Document, &json!({"path": 8}));
        assert_eq!(result.unwrap_err(), "Expected a document path");
        Ok(())
    }

    #[test]
    fn text_scan_continues_at_a_utf8_boundary_and_bytes_preserve_binary_data() -> Result<()> {
        let project = tempfile::tempdir()?;
        fs::write(project.path().join("text"), "汉汉汉")?;
        fs::write(project.path().join("binary"), [b'a', 0, 0xff, b'b'])?;
        let context = ContextData {
            root: project.path().to_path_buf(),
            grants: [Permission::ReadProject].into(),
            rule_paths: ApprovedRulePaths::default(),
            document: None,
            open_documents: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let ask = |kind, args| run(&context, kind, &args).0.map_err(anyhow::Error::msg);
        let found = ask(
            Query::ScanText,
            json!({"path":"text","contains":"汉汉","limit":1}),
        )?;
        assert_eq!(found["offsets"], json!([0]));
        assert_eq!(found["next_offset"], 3);
        assert_eq!(found["complete"], false);
        let more = ask(
            Query::ScanText,
            json!({"path":"text","contains":"汉汉","offset":found["next_offset"]}),
        )?;
        assert_eq!(more["offsets"], json!([3]));
        assert_eq!(more["complete"], true);
        assert_eq!(
            ask(Query::ReadFile, json!({"path":"text", "limit":1}))?,
            json!({"text":"汉", "next_offset":3, "complete":false})
        );
        assert!(ask(Query::ReadFile, json!({"path":"binary"})).is_err());
        assert!(ask(Query::ScanText, json!({"path":"binary","contains":"a"})).is_err());
        let bytes = ask(
            Query::ReadBytes,
            json!({"path":"binary","offset":1,"limit":2}),
        )?;
        assert_eq!(
            bytes,
            json!({"bytes":[0,255],"next_offset":3,"complete":false})
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn transient_sharing_error_retries_without_resetting_the_rule_deadline() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.txt");
        fs::write(&path, "ready")?;
        let hold = || fs::OpenOptions::new().write(true).share_mode(0).open(&path);
        let grant = || ContextData {
            root: directory.path().to_path_buf(),
            grants: [Permission::ReadProject].into(),
            rule_paths: ApprovedRulePaths::default(),
            document: None,
            open_documents: Vec::new(),
            deadline: Instant::now() + Duration::from_millis(250),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let arguments = json!({"path":"fixture.txt"});
        let lock = hold()?;
        let started = Instant::now();
        let (short, _) = run(&grant(), Query::ReadFile, &arguments);
        assert_eq!(short.unwrap_err(), "Rule data deadline expired");
        assert!((Duration::from_millis(100)..Duration::from_secs(1)).contains(&started.elapsed()));

        let context = ContextData {
            deadline: Instant::now() + Duration::from_secs(5),
            ..grant()
        };
        let mut held = Some(lock);
        let mut waits = Vec::new();
        let (data, _) = run_retries(&context, Query::ReadFile, &arguments, |wait| {
            waits.push(wait);
            if waits.len() == 2 {
                drop(held.take());
            }
        });
        assert_eq!(
            waits,
            [Duration::from_millis(100), Duration::from_millis(300)]
        );
        assert_eq!(data.unwrap()["text"], "ready");
        Ok(())
    }

    #[test]
    fn large_file_search_resumes_and_observes_changes() -> Result<()> {
        let project = tempfile::tempdir()?;
        let file = project.path().join("fixture.unknown");
        let mut output = File::create(&file)?;
        for _ in 0..384 {
            output.write_all(&[b'x'; 32 * 1024])?;
        }
        let crossing = 11 * 1024 * 1024 + 32767;
        output.seek(SeekFrom::Start(crossing))?;
        output.write_all("中match👋".as_bytes())?;
        output.sync_all()?;
        let context = ContextData {
            root: project.path().to_path_buf(),
            grants: [Permission::ReadProject].into(),
            rule_paths: ApprovedRulePaths::default(),
            document: None,
            open_documents: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let (data, dependency) = run(
            &context,
            Query::ScanText,
            &json!({
                "path": "fixture.unknown", "contains": "中match👋", "offset": 11 * 1024 * 1024
            }),
        );
        let data = data.map_err(anyhow::Error::msg)?;
        assert_eq!(data["offsets"], json!([crossing]));
        assert_eq!(data["complete"], true);
        let absolute = file.canonicalize()?;
        let (bytes, _) = run(
            &context,
            Query::ReadBytes,
            &json!({"path":absolute, "offset":crossing, "limit":3}),
        );
        assert_eq!(bytes.unwrap()["bytes"], json!("中".as_bytes()));
        let (missing, _) = run(
            &context,
            Query::ScanText,
            &json!({"path":"fixture.unknown", "contains":"no match here"}),
        );
        let missing = missing.unwrap();
        assert_eq!(missing["offsets"], json!([]));
        assert_eq!(missing["complete"], true);
        let near_deadline = ContextData {
            deadline: Instant::now() + Duration::from_millis(10),
            ..context
        };
        let incomplete = scan(File::open(&file)?, "no match here", 0, 32, &near_deadline)?;
        assert_eq!(
            incomplete,
            json!({"offsets": [], "next_offset": 0, "complete": false})
        );
        let dependencies = [dependency.expect("file dependency")];
        assert!(files_current(&dependencies));
        output.set_len(0)?;
        assert!(!files_current(&dependencies));
        Ok(())
    }
}
