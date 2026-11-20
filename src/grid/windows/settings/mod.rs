//! Settings dialog with a capability tree, typed form controls and a session-wide draft.

mod caret;
mod input;
mod selector;
mod view;

use crate::{
    grid::{
        mnemonic::Mnemonic,
        render::{Metrics, Panel},
        text_field,
    },
    ui::ime::Composition,
};
use gpui::{App, AppContext, Entity};
use nanobug::{animation::scroll::GridScroll, i18n};
use serde_json::Value;
use std::collections::HashSet;

pub(in crate::grid) use caret::CursorRegion;

/// A stable stored choice and its localized caption.
#[derive(Clone, Debug)]
pub(in crate::grid) struct Choice {
    pub id: String,
    pub label: String,
}

/// The shape of an editable setting, including constraints used by both input and painting.
#[derive(Clone, Debug)]
pub(in crate::grid) enum Kind {
    /// Read-only diagnostics for an owner that cannot load its configuration.
    Notice,
    Toggle,
    Select(Vec<Choice>),
    Multi(Vec<Choice>),
    Number {
        min: i64,
        max: i64,
    },
    /// A missing numeric override inherits its parent's preset value.
    OptionalNumber {
        min: i64,
        max: i64,
    },
    Decimal {
        min: i64,
        max: i64,
    },
    Text,
    /// Optional RGB foreground with a live preview of the inherited theme color.
    Color {
        inherited: Option<u32>,
    },
}

/// An opening-time read of one setting and its system default.
#[derive(Clone, Debug)]
pub(in crate::grid) struct Field {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    pub value: Value,
    pub default: Value,
}

/// A single apply scope, located by its stable ID and localized tree path.
#[derive(Clone, Debug)]
pub(in crate::grid) struct Page {
    pub id: String,
    pub path: Vec<String>,
    pub fields: Vec<Field>,
}

/// Only these actions leave the window model and can perform persistent writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum Action {
    Cancel,
    Apply,
    Confirm,
}

/// Values required by one backend write, in the same order as the form.
pub(in crate::grid) type Changes = Vec<(String, Value)>;

/// Keep the open-time configuration immutable, independently of later successful applications.
struct Form {
    page: Page,
    draft: Vec<Value>,
    applied: Vec<Value>,
    /// An explicit project override can take the same effective value as its original inheritance.
    storage_changes: HashSet<String>,
}

impl Form {
    fn new(page: Page) -> Self {
        let draft: Vec<_> = page
            .fields
            .iter()
            .map(|field| field.value.clone())
            .collect();
        Self {
            applied: draft.clone(),
            draft,
            page,
            storage_changes: HashSet::new(),
        }
    }

    /// Compare against successful writes, so cancel can undo changes made by an earlier Apply.
    fn changes(&self, action: Action) -> Changes {
        self.page
            .fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| {
                let target = if action == Action::Cancel {
                    &field.value
                } else {
                    &self.draft[index]
                };
                (target != &self.applied[index]
                    || (action == Action::Cancel && self.storage_changes.contains(&field.id)))
                .then(|| (field.id.clone(), target.clone()))
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Tree,
    Form,
}

/// Transient changes in a dropdown or text editor are committed to the form only on completion.
enum Editor {
    Input {
        text: String,
        field: text_field::Field,
    },
    List {
        cursor: usize,
        value: Value,
    },
}

#[derive(Clone, Copy)]
enum Button {
    Cancel,
    Reset,
    Restore,
    Apply,
    Confirm,
}

const BUTTONS: [(Button, &str); 5] = [
    (Button::Cancel, "settings.cancel"),
    (Button::Reset, "settings.reset"),
    (Button::Restore, "settings.restore"),
    (Button::Apply, "settings.apply"),
    (Button::Confirm, "settings.confirm"),
];

fn buttons() -> impl Iterator<Item = (Button, Mnemonic)> {
    BUTTONS
        .into_iter()
        .map(|(button, key)| (button, Mnemonic::parse(i18n::text(key))))
}

/// A stable directory node. Parent links keep the hierarchy independent of backend page order.
struct Node {
    path: Vec<String>,
    parent: Option<usize>,
    page: Option<usize>,
    branch: bool,
}

impl Node {
    /// Add missing ancestors for a page without disturbing any existing node indices.
    fn add_page(nodes: &mut Vec<Self>, page: &Page, index: usize) {
        let mut parent = None;
        for depth in 1..=page.path.len() {
            let path = &page.path[..depth];
            let id = nodes
                .iter()
                .position(|node| node.path == path)
                .unwrap_or_else(|| {
                    nodes.push(Self {
                        path: path.to_vec(),
                        parent,
                        page: None,
                        branch: false,
                    });
                    nodes.len() - 1
                });
            if depth == page.path.len() {
                nodes[id].page = Some(index);
            } else {
                nodes[id].branch = true;
            }
            parent = Some(id);
        }
    }
}

/// Own the selected directory, controls, draft and the scroll positions of each viewport.
pub(in crate::grid) struct Settings {
    forms: Vec<Form>,
    nodes: Vec<Node>,
    expanded: HashSet<usize>,
    tree_row: usize,
    page: usize,
    row: usize,
    pane: Pane,
    editor: Option<Editor>,
    error: String,
    pub(in crate::grid) composition: Composition,
    tree_scroll: GridScroll,
    form_scroll: GridScroll,
    selector_scroll: GridScroll,
    wheel_remainder: [f32; 3],
    reveal: bool,
    panels: [Entity<Panel>; 5],
}

impl Settings {
    /// Take the opening snapshot once, before any control can change an owner.
    pub(in crate::grid) fn new(mut pages: Vec<Page>, metrics: Metrics, cx: &mut App) -> Self {
        if pages.is_empty() {
            pages.push(Page {
                id: String::new(),
                path: vec![i18n::text("settings.title").into()],
                fields: vec![],
            });
        }
        let page = pages
            .iter()
            .position(|p| p.id == "core.vim.indent")
            .unwrap_or(0);
        let mut nodes = Vec::<Node>::new();
        for (index, page) in pages.iter().enumerate() {
            Node::add_page(&mut nodes, page, index);
        }
        let tree_row = nodes
            .iter()
            .position(|node| node.page == Some(page))
            .unwrap_or(0);
        let expanded = nodes
            .iter()
            .enumerate()
            .filter_map(|(id, node)| {
                (node.parent.is_none() || pages[page].path.starts_with(&node.path)).then_some(id)
            })
            .collect();
        Self {
            forms: pages.into_iter().map(Form::new).collect(),
            nodes,
            expanded,
            tree_row,
            page,
            row: 0,
            pane: Pane::Tree,
            editor: None,
            error: String::new(),
            composition: Composition::default(),
            tree_scroll: GridScroll::default(),
            form_scroll: GridScroll::default(),
            selector_scroll: GridScroll::default(),
            wheel_remainder: [0.; 3],
            reveal: true,
            panels: std::array::from_fn(|_| cx.new(|_| Panel::new(metrics))),
        }
    }

    pub(in crate::grid) fn pages(&self) -> impl Iterator<Item = &Page> {
        self.forms.iter().map(|form| &form.page)
    }

    /// Attach newly available backend pages without changing the open session's existing snapshots.
    pub(in crate::grid) fn append_pages(&mut self, pages: Vec<Page>) {
        let previous_len = self.nodes.len();
        for page in pages {
            if self.forms.iter().any(|form| form.page.id == page.id) {
                continue;
            }
            Node::add_page(&mut self.nodes, &page, self.forms.len());
            self.forms.push(Form::new(page));
        }
        self.expanded.extend(
            self.nodes
                .iter()
                .enumerate()
                .skip(previous_len)
                .filter_map(|(i, n)| n.parent.is_none().then_some(i)),
        );
    }

    /// Gather writes across every visited page or only the currently displayed one for Apply.
    pub(in crate::grid) fn plan(&self, action: Action) -> Vec<(Page, Changes)> {
        self.forms
            .iter()
            .enumerate()
            .filter(|(index, _)| action != Action::Apply || *index == self.page)
            .filter_map(|(_, form)| {
                let changes = form.changes(action);
                (!changes.is_empty()).then(|| (form.page.clone(), changes))
            })
            .collect()
    }

    /// Snapshot the successful values of a single write for compensating earlier page writes.
    pub(in crate::grid) fn applied_changes(
        &self,
        id: &str,
        changes: &[(String, Value)],
    ) -> Changes {
        let Some(form) = self.forms.iter().find(|form| form.page.id == id) else {
            return vec![];
        };
        changes
            .iter()
            .filter_map(|(key, _)| {
                let index = form.page.fields.iter().position(|field| field.id == *key)?;
                Some((key.clone(), form.applied[index].clone()))
            })
            .collect()
    }

    /// Record values only after their owner confirms the write.
    pub(in crate::grid) fn mark_written(&mut self, id: &str, changes: &[(String, Value)]) {
        let Some(form) = self.forms.iter_mut().find(|form| form.page.id == id) else {
            return;
        };
        for (key, value) in changes {
            if let Some(index) = form.page.fields.iter().position(|field| field.id == *key) {
                form.applied[index] = value.clone();
            }
        }
    }

    /// Reflect a newly inherited value in clean rows, retaining independent drafts and the
    /// original Cancel baseline. Text currently being typed lives in a separate transient editor.
    pub(in crate::grid) fn refresh_inherited_value(
        &mut self,
        page_id: &str,
        field_id: &str,
        value: Value,
    ) {
        let Some(form) = self.forms.iter_mut().find(|form| form.page.id == page_id) else {
            return;
        };
        let Some(index) = form
            .page
            .fields
            .iter()
            .position(|field| field.id == field_id)
        else {
            return;
        };
        if form.draft[index] == form.applied[index] {
            form.draft[index] = value.clone();
        }
        form.applied[index] = value;
    }

    /// Refresh inherited controls after applying a parent style, preserving independent drafts and Cancel history.
    pub(in crate::grid) fn refresh_highlight_pages(&mut self, pages: Vec<Page>) {
        for page in pages {
            let Some(form) = self.forms.iter_mut().find(|f| f.page.id == page.id) else {
                continue;
            };
            for (i, (current, field)) in form.page.fields.iter_mut().zip(page.fields).enumerate() {
                debug_assert_eq!(current.id, field.id);
                if form.draft[i] == form.applied[i] {
                    form.draft[i] = field.value.clone();
                }
                form.applied[i] = field.value;
                current.default = field.default;
                current.kind = field.kind;
            }
        }
    }

    /// Include a changed override representation in Cancel even if effective values compare equal.
    pub(in crate::grid) fn mark_storage_changed(
        &mut self,
        page_id: &str,
        field_id: &str,
        changed: bool,
    ) {
        if let Some(form) = self.forms.iter_mut().find(|form| form.page.id == page_id) {
            if changed {
                form.storage_changes.insert(field_id.to_owned());
            } else {
                form.storage_changes.remove(field_id);
            }
        }
    }

    /// Rebind captions after applying a language change, retaining edits and the open-time snapshot.
    pub(in crate::grid) fn relabel(&mut self, pages: Vec<Page>) {
        for node in &mut self.nodes {
            if let Some(page) = self
                .forms
                .iter()
                .find(|form| form.page.path.starts_with(&node.path))
                && let Some(translated) = pages.iter().find(|p| p.id == page.page.id)
                && translated.path.len() >= node.path.len()
            {
                node.path = translated.path[..node.path.len()].to_vec();
            }
        }
        for form in &mut self.forms {
            if let Some(translated) = pages.iter().find(|p| p.id == form.page.id) {
                form.page.path.clone_from(&translated.path);
                for field in &mut form.page.fields {
                    if let Some(other) = translated.fields.iter().find(|f| f.id == field.id) {
                        field.kind.clone_from(&other.kind);
                        field.label.clone_from(&other.label);
                    }
                }
            }
        }
    }

    pub(in crate::grid) fn set_error(&mut self, error: String) {
        self.error = error;
    }

    #[cfg(test)]
    pub(in crate::grid) fn selected_tree_path(&self) -> &[String] {
        &self.nodes[self.tree_row].path
    }

    #[cfg(test)]
    pub(in crate::grid) fn selected_page_id(&self) -> &str {
        &self.forms[self.page].page.id
    }

    #[cfg(test)]
    pub(in crate::grid) fn selected_field_id(&self) -> Option<&str> {
        self.field().map(|field| field.id.as_str())
    }

    /// Enumerate only visible nodes in tree order, even when backend pages interleave subtrees.
    fn visible_tree(&self) -> Vec<usize> {
        fn children(settings: &Settings, parent: Option<usize>, out: &mut Vec<usize>) {
            for (id, node) in settings
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.parent == parent)
            {
                out.push(id);
                if node.branch && settings.expanded.contains(&id) {
                    children(settings, Some(id), out);
                }
            }
        }
        let mut out = Vec::new();
        children(self, None, &mut out);
        out
    }

    fn field(&self) -> Option<&Field> {
        self.forms[self.page].page.fields.get(self.row)
    }

    fn clear_editor(&mut self) {
        self.editor = None;
        self.selector_scroll = GridScroll::default();
        self.wheel_remainder[2] = 0.;
        self.composition.clear();
        self.error.clear();
    }
}
