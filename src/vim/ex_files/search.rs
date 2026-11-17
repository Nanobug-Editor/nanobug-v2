//! Search for local files using the ordered directories in the Vim 'path' option.

use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
};

/// Resolve the one-based `occurrence` of `name` in the ordered search directories.
pub fn find_file(
    root: &Path,
    current: Option<&Path>,
    options: &str,
    name: &str,
    occurrence: usize,
) -> Option<PathBuf> {
    find_path(root, current, options, name, occurrence, Path::is_file)
}

/// Find a relative directory using the same traversal as 'path'.
pub fn find_directory(root: &Path, options: &str, name: &str) -> Option<PathBuf> {
    find_path(root, None, options, name, 1, Path::is_dir)
}

/// Traverse the ordered search paths and return the requested distinct matching path.
fn find_path(
    root: &Path,
    current: Option<&Path>,
    options: &str,
    name: &str,
    occurrence: usize,
    is_match: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if name.is_empty() || occurrence == 0 {
        return None;
    }
    let name = expand_home(name);
    let name = Path::new(&name);
    let mut matches = HashSet::new();
    let mut found = None;
    let mut visit = |directory: &Path| {
        let path = directory.join(name);
        if path.components().any(|component| matches!(component, Component::Normal(name) if name.to_string_lossy().contains(['*', '?']))) {
            let components: Vec<_> = path.components().collect();
            visit_directories(PathBuf::new(), &components, &mut |path| {
                if is_match(path)
                    && let Ok(path) = path.canonicalize()
                    && matches.insert(path.clone())
                    && matches.len() == occurrence
                {
                    found = Some(path);
                    return true;
                }
                false
            })
        } else {
            if is_match(&path)
                && let Ok(path) = path.canonicalize()
                && matches.insert(path.clone())
                && matches.len() == occurrence
            {
                found = Some(path);
                true
            } else {
                false
            }
        }
    };
    let explicit = name.has_root()
        || matches!(
            name.components().next(),
            Some(Component::CurDir | Component::ParentDir)
        );
    if explicit {
        visit(root);
        return found;
    }
    let file_dir = current.and_then(Path::parent).unwrap_or(root);
    for entry in path_entries(options) {
        let (directory, stop) = entry
            .split_once(';')
            .map_or((entry.as_str(), None), |(path, stop)| (path, Some(stop)));
        let directory = expand_home(directory);
        let prefix =
            if directory == "." || directory.starts_with("./") || directory.starts_with(".\\") {
                file_dir
            } else {
                root
            };
        let directory = Path::new(&directory);
        let stop = stop
            .filter(|stop| !stop.is_empty())
            .map(|stop| root.join(stop));
        for base in prefix.ancestors() {
            let pattern = base.join(directory);
            let components: Vec<_> = pattern.components().collect();
            if visit_directories(PathBuf::new(), &components, &mut visit) {
                return found;
            }
            if !entry.contains(';') || stop.as_deref() == Some(base) || directory.is_absolute() {
                break;
            }
        }
    }
    found
}

/// Accept path list commas, ordinary spaces and backslash-escaped separators.
fn path_entries(options: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut buffer = String::new();
    let mut chars = options.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && chars
                .peek()
                .is_some_and(|next| *next == ',' || next.is_whitespace())
        {
            buffer.push(chars.next().expect("escaped separator"));
        } else if ch == ',' || ch.is_whitespace() && !buffer.is_empty() {
            parts.push(std::mem::take(&mut buffer));
        } else if !ch.is_whitespace() {
            buffer.push(ch);
        }
    }
    if !buffer.is_empty() || options.is_empty() || options.ends_with(',') {
        parts.push(buffer);
    }
    parts
}

/// Expand an explicitly home-relative path using the native user profile directory.
fn expand_home(text: &str) -> String {
    if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        if let Some(home) = std::env::var_os(variable) {
            return PathBuf::from(home)
                .join(text.get(2..).unwrap_or(""))
                .to_string_lossy()
                .into_owned();
        }
    }
    text.to_owned()
}

/// Visit directories with the given literal, wildcard, and recursive components.
fn visit_directories(
    directory: PathBuf,
    components: &[Component<'_>],
    visit: &mut impl FnMut(&Path) -> bool,
) -> bool {
    let Some((component, suffix)) = components.split_first() else {
        return visit(&directory);
    };
    let Component::Normal(component) = *component else {
        return visit_directories(directory.join(component.as_os_str()), suffix, visit);
    };
    let component = component.to_string_lossy();
    if let Some(depth) = component.strip_prefix("**").and_then(|limit| {
        if limit.is_empty() {
            Some(30)
        } else {
            limit.parse::<usize>().ok()
        }
    }) {
        let mut pending = VecDeque::from([(directory, 0)]);
        while let Some((path, level)) = pending.pop_front() {
            if visit_directories(path.clone(), suffix, visit) {
                return true;
            }
            if level < depth {
                pending.extend(children(&path).into_iter().map(|child| (child, level + 1)));
            }
        }
        false
    } else if component.contains(['*', '?']) {
        let expression = regex::escape(&component)
            .replace("\\*", ".*")
            .replace("\\?", ".");
        let case = if cfg!(windows) { "(?i)" } else { "" };
        let pattern =
            regex::Regex::new(&format!("{case}^{expression}$")).expect("escaped path expression");
        matches_at(&directory, !suffix.is_empty())
            .into_iter()
            .any(|path| {
                path.file_name()
                    .is_some_and(|name| pattern.is_match(&name.to_string_lossy()))
                    && visit_directories(path, suffix, visit)
            })
    } else {
        visit_directories(directory.join(components[0].as_os_str()), suffix, visit)
    }
}

/// Enumerate ordinary child directories in a stable order with no symlink recursion.
fn children(path: &Path) -> Vec<PathBuf> {
    matches_at(path, true)
}

/// Enumerate sorted paths available to a wildcard component.
fn matches_at(path: &Path, directories_only: bool) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| !directories_only || entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Observe file-relative, working directory, and recursive lookup using real files.
    #[test]
    fn search_path_uses_scope_and_ignores_duplicate_matches() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let absolute = folder.path().canonicalize()?;
        let root = absolute.as_path();
        let source = root.join("src");
        fs::create_dir_all(source.join("nested"))?;
        fs::write(root.join("match.rs"), "root")?;
        fs::write(source.join("match.rs"), "src")?;
        fs::write(source.join("nested/match.rs"), "nested")?;
        let current = source.join("main.rs");
        assert_eq!(
            find_file(root, Some(&current), ".,,**", "match.rs", 1),
            Some(source.join("match.rs").canonicalize()?)
        );
        assert_eq!(
            find_file(root, Some(&current), ".,,**", "match.rs", 2),
            Some(root.join("match.rs").canonicalize()?)
        );
        assert_eq!(
            find_file(root, Some(&current), ".,,**", "match.rs", 3),
            Some(source.join("nested/match.rs").canonicalize()?)
        );
        assert!(find_file(root, Some(&current), ".,,**", "match.rs", 4).is_none());
        assert_eq!(
            find_file(root, Some(&current), ".", "./match.rs", 1),
            Some(root.join("match.rs").canonicalize()?)
        );
        Ok(())
    }
}
