//! Keep file argument navigation independent of the editor's open buffer order.

use super::{Workspace, editor_splits::PaneId, layout::Focus};
use gpui::{Context, Window};
use nanobug::{i18n, vim::ArgumentCommand};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

/// Project-wide and window-local file argument lists.
#[derive(Default)]
pub(super) struct ArgumentList {
    /// Default scope shared by Session windows.
    global: Arguments,
    /// Windows sharing one local argument list after a split.
    local: HashMap<PaneId, usize>,
    /// Lists still referenced by one or more windows.
    lists: HashMap<usize, Arguments>,
    next: usize,
}

#[derive(Default)]
struct Arguments {
    /// Files in argument order, including intentionally duplicated entries.
    files: Vec<PathBuf>,
    /// Zero-based argument positions for panes that have navigated the list.
    positions: HashMap<PaneId, usize>,
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    /// Split windows share a local list until a window chooses another scope.
    #[test]
    fn local_argument_lists_inherit_and_detach_independently() {
        let mut list = ArgumentList::default();
        list.push_startup_file("global.txt".into());
        list.set_scope(0, true, None);
        assert_eq!(list.get(0, 0), Some(Path::new("global.txt")));
        list.inherit_pane(0, 1);
        list.replace_for_pane(1, vec!["local.txt".into()]);
        assert_eq!(list.get(0, 0), Some(Path::new("local.txt")));
        assert_eq!(list.get(2, 0), Some(Path::new("global.txt")));
        list.set_scope(1, false, None);
        list.replace_for_pane(1, vec!["changed.txt".into()]);
        assert_eq!(list.get(2, 0), Some(Path::new("changed.txt")));
        assert_eq!(list.get(0, 0), Some(Path::new("local.txt")));
        list.forget_pane(0);
        assert!(list.lists.is_empty());
    }
}

impl ArgumentList {
    fn for_pane(&self, pane: PaneId) -> &Arguments {
        self.local
            .get(&pane)
            .map_or(&self.global, |id| &self.lists[id])
    }

    fn for_pane_mut(&mut self, pane: PaneId) -> &mut Arguments {
        match self.local.get(&pane) {
            Some(id) => self.lists.get_mut(id).expect("live argument list"),
            None => &mut self.global,
        }
    }

    /// Switch scope without opening a file. A fresh local list copies the global list.
    pub(super) fn set_scope(&mut self, pane: PaneId, local: bool, path: Option<&Path>) {
        self.forget_pane(pane);
        if local {
            self.next += 1;
            self.local.insert(pane, self.next);
            self.lists.insert(
                self.next,
                Arguments {
                    files: self.global.files.clone(),
                    ..Arguments::default()
                },
            );
        }
        let state = self.for_pane_mut(pane);
        if let Some(index) = path.and_then(|path| state.files.iter().position(|file| file == path))
        {
            state.positions.insert(pane, index);
        }
    }

    /// Access an editor's current argument files in their original order.
    pub(super) fn files(&self, pane: PaneId) -> &[PathBuf] {
        &self.for_pane(pane).files
    }

    /// Number of file entries to visit in an Ex argument batch, including repeats.
    pub(super) fn len(&self, pane: PaneId) -> usize {
        self.files(pane).len()
    }

    /// Append a canonical startup file without bringing it into an editor buffer.
    pub(super) fn push_startup_file(&mut self, file: PathBuf) {
        self.global.files.push(file);
    }

    /// Return the zero-based argument for `pane`.
    pub(super) fn index(&self, pane: PaneId) -> usize {
        self.for_pane(pane)
            .positions
            .get(&pane)
            .copied()
            .unwrap_or(0)
    }

    /// Read the path at a zero-based argument position.
    pub(super) fn get(&self, pane: PaneId, index: usize) -> Option<&Path> {
        self.files(pane).get(index).map(PathBuf::as_path)
    }

    /// Remember an argument selected in a new split.
    pub(super) fn select(&mut self, pane: PaneId, index: usize) {
        let list = self.for_pane_mut(pane);
        debug_assert!(index < list.files.len());
        list.positions.insert(pane, index);
    }

    /// Replace the shared list after a split has successfully opened its first path.
    pub(super) fn replace_for_pane(&mut self, pane: PaneId, files: Vec<PathBuf>) {
        let list = self.for_pane_mut(pane);
        list.files = files;
        list.positions.clear();
        self.select(pane, 0);
    }

    /// Inherit the current argument when splitting `source` into editor `destination`.
    pub(super) fn inherit_pane(&mut self, source: PaneId, destination: PaneId) {
        if let Some(&list) = self.local.get(&source) {
            self.local.insert(destination, list);
        }
        if !self.files(source).is_empty() {
            self.select(destination, self.index(source));
        }
    }

    /// Forget navigation state for the closed editor `pane`.
    pub(super) fn forget_pane(&mut self, pane: PaneId) {
        self.for_pane_mut(pane).positions.remove(&pane);
        if let Some(id) = self.local.remove(&pane)
            && !self.local.values().any(|value| *value == id)
        {
            self.lists.remove(&id);
        }
    }
}

impl Arguments {
    /// Remove arguments selected by `remove` and keep other panes on their retained entries.
    fn remove_matching(&mut self, mut remove: impl FnMut(usize, &Path) -> bool) {
        let mut retained = 0;
        let mapped: Vec<_> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let next = retained;
                let keep = !remove(index, file);
                retained += usize::from(keep);
                (next, keep)
            })
            .collect();
        let mut index = 0;
        self.files.retain(|_| {
            let keep = mapped[index].1;
            index += 1;
            keep
        });
        self.positions.retain(|_, index| {
            if retained == 0 {
                return false;
            }
            *index = mapped[*index].0.min(retained - 1);
            true
        });
    }
}

impl Workspace {
    /// Execute an argument-list `command` with the optional `count` and Ex `arguments`.
    /// Uses `window` and `cx` to reveal any newly opened buffer in its original pane.
    pub(super) fn argument_list_command(
        &mut self,
        command: ArgumentCommand,
        count: Option<usize>,
        arguments: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ArgumentCommand::*;
        let now = self.scroll_epoch.elapsed().as_secs_f64();
        let directory = self.ex_current_dir().to_owned();
        let project = self.projects.active_mut();
        let pane = project.active_editor_id();
        let active = project.editor.vim().doc.path.clone();
        let index = project.arguments.index(pane);
        let list = project.arguments.for_pane_mut(pane);
        if !list.files.is_empty() {
            list.positions.entry(pane).or_insert(index);
        }
        let names = argument_words(&arguments);
        let count_arg = match names.as_slice() {
            [value] => value.parse::<usize>().ok(),
            _ => None,
        };
        let mut selected = None;
        match command {
            List if names.is_empty() => {
                project.status = if list.files.is_empty() {
                    i18n::text("vim_bracket.no_arguments").into()
                } else {
                    list.files
                        .iter()
                        .enumerate()
                        .map(|(cursor, path)| {
                            let name = path.strip_prefix(&project.root).unwrap_or(path).display();
                            if cursor == index && Some(path) == active.as_ref() {
                                format!("[{name}]")
                            } else {
                                name.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("  ")
                };
                return;
            }
            List | Step { forward: true }
                if !names.is_empty() && (command == List || count_arg.is_none()) =>
            {
                let files = argument_paths(&names, &directory, active.as_deref());
                let Some(first) = files.first() else {
                    project.status = i18n::text("vim_bracket.no_arguments").into();
                    return;
                };
                // Load before replacing the list so a bad first path leaves it intact.
                match project.jump_file(first, false, now, |doc| Some(doc.cursor)) {
                    Ok(_) => {
                        for file in &files {
                            project.list_unloaded_file(file.clone(), None);
                        }
                        project.arguments.replace_for_pane(pane, files);
                    }
                    Err(error) => {
                        project.status = format!("{error:#}");
                        return;
                    }
                }
                project.focus_pane(Focus::Editor);
                project.status.clear();
                self.reset_tab_view(window, cx);
                return;
            }
            Add { edit } => {
                let files = if names.is_empty() {
                    active.into_iter().collect()
                } else {
                    argument_paths(&names, &directory, active.as_deref())
                };
                if files.is_empty() {
                    project.status = i18n::text("vim_bracket.no_arguments").into();
                    return;
                }
                let insert = count.unwrap_or(index + 1).min(list.files.len());
                for position in list.positions.values_mut() {
                    if *position >= insert {
                        *position += files.len();
                    }
                }
                list.files.splice(insert..insert, files);
                if edit {
                    selected = Some(insert);
                } else {
                    project.status.clear();
                    return;
                }
            }
            Delete => {
                if names.is_empty() {
                    let requested = count.map_or(index, |value| value.saturating_sub(1));
                    list.remove_matching(|cursor, _| cursor == requested);
                } else {
                    let root = &directory;
                    let matches: Vec<_> = names
                        .iter()
                        .map(|name| {
                            let pattern = normalize_separators(name);
                            (
                                wildcard(&pattern),
                                root.join(name)
                                    .canonicalize()
                                    .unwrap_or_else(|_| root.join(name)),
                            )
                        })
                        .collect();
                    list.remove_matching(|cursor, path| {
                        let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
                        let relative = normalize_separators(&relative);
                        names.iter().zip(&matches).any(|(name, (wildcard, exact))| {
                            if name == "%" {
                                cursor == index
                            } else if let Some(wildcard) = wildcard {
                                wildcard.is_match(&relative)
                            } else {
                                exact == path
                            }
                        })
                    });
                }
                project.status.clear();
                return;
            }
            Deduplicate => {
                let active_arguments: Vec<_> = list
                    .positions
                    .iter()
                    .map(|(&pane, &index)| (pane, list.files[index].clone()))
                    .collect();
                let mut unique = HashSet::new();
                list.remove_matching(|_, path| !unique.insert(path.to_owned()));
                for (pane, file) in active_arguments {
                    if let Some(index) = list.files.iter().position(|path| *path == file) {
                        list.positions.insert(pane, index);
                    }
                }
                project.status.clear();
                return;
            }
            Step { forward } if names.is_empty() || count_arg.is_some() => {
                let steps = count.or(count_arg).unwrap_or(1);
                selected = if forward {
                    index.checked_add(steps)
                } else {
                    index.checked_sub(steps)
                };
            }
            At if names.is_empty() || count_arg.is_some() => {
                selected = count
                    .or(count_arg)
                    .map_or(Some(index), |value| value.checked_sub(1));
            }
            Edge { last } if names.is_empty() => {
                selected = if last {
                    list.files.len().checked_sub(1)
                } else {
                    Some(0)
                };
            }
            _ => {}
        }
        let Some((index, file)) =
            selected.and_then(|index| list.files.get(index).cloned().map(|file| (index, file)))
        else {
            project.status = i18n::text(if list.files.is_empty() {
                "vim_bracket.no_arguments"
            } else {
                "vim_bracket.argument_bounds"
            })
            .into();
            return;
        };
        if let Err(error) = project.jump_file(&file, false, now, |doc| Some(doc.cursor)) {
            project.status = format!("{error:#}");
            return;
        }
        project.arguments.select(pane, index);
        project.focus_pane(Focus::Editor);
        project.status.clear();
        self.reset_tab_view(window, cx);
    }
}

/// Parse whitespace-separated Ex paths, preserving escaped spaces and Windows separators.
pub(super) fn argument_words(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|ch| ch.is_whitespace()) {
            word.push(chars.next().expect("peeked whitespace"));
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        result.push(word);
    }
    result
}

/// Resolve `names` against `root`, expanding `%` to `active` and sorting each wildcard expansion.
pub(super) fn argument_paths(names: &[String], root: &Path, active: Option<&Path>) -> Vec<PathBuf> {
    names
        .iter()
        .flat_map(|name| {
            if name == "%" {
                return active.into_iter().map(Path::to_owned).collect::<Vec<_>>();
            }
            let path = root.join(name);
            let mut paths = vec![PathBuf::new()];
            for component in path.components() {
                let part = component.as_os_str();
                if let Component::Normal(_) = component
                    && let Some(matcher) = part.to_str().and_then(wildcard)
                {
                    paths = paths
                        .iter()
                        .flat_map(|dir| fs::read_dir(dir).into_iter().flatten())
                        .filter_map(Result::ok)
                        .filter(|entry| matcher.is_match(&entry.file_name().to_string_lossy()))
                        .map(|entry| entry.path())
                        .collect();
                } else {
                    for existing in &mut paths {
                        existing.push(part);
                    }
                }
            }
            paths.sort();
            paths
                .into_iter()
                .map(|path| path.canonicalize().unwrap_or(path))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Compile portable `*` and `?` wildcards in `pattern`, when either is present.
fn wildcard(pattern: &str) -> Option<regex::Regex> {
    if !pattern.contains(['*', '?']) {
        return None;
    }
    let pattern = regex::escape(pattern)
        .replace("\\*", ".*")
        .replace("\\?", ".");
    let case = if cfg!(windows) { "(?i)" } else { "" };
    regex::Regex::new(&format!("{case}^{pattern}$")).ok()
}

/// Normalize Windows path separators before comparing a relative filename with a wildcard.
fn normalize_separators(path: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) && path.contains('\\') {
        path.replace('\\', "/").into()
    } else {
        path.into()
    }
}
