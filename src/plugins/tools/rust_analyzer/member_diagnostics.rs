//! Supplement Rust's native reports with module members it classifies as unresolved.

use super::*;
use nanobug_plugin_sdk::language::{ByteRange, SyntaxKind};

/// A resolved module, optionally followed by comments and a path separator.
#[derive(Default)]
pub(super) struct ModulePath {
    module: Option<ByteRange>,
    consumed: usize,
    separator: bool,
}

impl ModulePath {
    /// Consume one semantic token and return its module qualifier when the path is complete.
    pub(super) fn qualifier<'a>(
        &mut self,
        text: &'a str,
        range: ByteRange,
        kind: Option<SyntaxKind>,
    ) -> Option<&'a str> {
        if kind == Some(SyntaxKind::Module) {
            self.module = Some(range);
            self.consumed = range.end;
            self.separator = false;
            return None;
        }
        let module = self.module.take()?;
        // Depending on the server's highlighting settings, the separator may be an uncolored gap.
        match text.get(self.consumed..range.start)?.trim() {
            "" => {}
            "::" if !self.separator => self.separator = true,
            _ => return None,
        }
        if kind != Some(SyntaxKind::Comment) {
            if self.separator || &text[range.start..range.end] != "::" {
                return self.separator.then(|| &text[module.start..module.end]);
            }
            self.separator = true;
        }
        self.module = Some(module);
        self.consumed = range.end;
        None
    }
}

/// The two server replies can arrive in either order and are discarded together on an edit.
#[derive(Default)]
pub(super) struct RustDiagnostics {
    native: Option<Vec<Value>>,
    members: Vec<Value>,
    /// Cargo reads this disk text while native analysis reads the editor overlay.
    pub(super) disk: Option<Arc<str>>,
    pub(super) compiler: Vec<Value>,
    /// A previous Cargo invocation was still active when the baseline changed.
    pub(super) awaiting_check: bool,
    pub(super) entry: Option<Value>,
    /// A module or library cannot acquire an entry requirement until the project model changes.
    pub(super) binary_root: Option<bool>,
}

impl Open {
    /// Publish native diagnostics immediately, including any known unresolved Rust members.
    pub(super) fn report_diagnostics(&mut self, items: Vec<Value>, event: &impl Fn(Event)) {
        if let Some(rust) = &mut self.rust_diagnostics {
            rust.native = Some(items);
            rust.publish(&self.snapshot, event);
        } else {
            event(Event::Diagnostics {
                snapshot: self.snapshot.clone(),
                items,
            });
        }
    }
}

impl RustDiagnostics {
    /// Keep disk reports for undo, but reset every overlay-derived result on an edit.
    pub(super) fn next_revision(&mut self) -> Self {
        Self {
            disk: self.disk.clone(),
            compiler: std::mem::take(&mut self.compiler),
            binary_root: self.binary_root,
            awaiting_check: self.awaiting_check,
            ..Default::default()
        }
    }

    /// A save may be immediately followed by another edit before the worker wakes.
    pub(super) fn saved(&mut self, text: Option<String>) {
        if self.disk.as_deref() != text.as_deref() {
            self.compiler.clear();
            self.disk = text.map(Into::into);
        }
    }

    /// Rust-analyzer pushes Cargo results separately from its native diagnostic pull.
    pub(super) fn pushed(
        &mut self,
        items: Vec<Value>,
        pull: bool,
        versioned: bool,
        snapshot: &Arc<DocumentSnapshot>,
        event: &impl Fn(Event),
    ) {
        let (compiler, native) = items
            .into_iter()
            .partition(compiler_diagnostics::from_compiler);
        if !self.awaiting_check {
            self.compiler = compiler
        }
        if !pull && versioned {
            self.native = Some(native);
        }
        self.publish(snapshot, event);
    }

    /// Recheck the merged report when a semantic refresh changes the unresolved members.
    pub(super) fn update_members(
        &mut self,
        members: Vec<Value>,
        snapshot: &Arc<DocumentSnapshot>,
        event: &impl Fn(Event),
    ) {
        if self.members != members {
            self.members = members;
            self.publish(snapshot, event);
        }
    }

    /// Prefer the server's explanation on overlapping errors and ignore inactive cfg regions.
    pub(super) fn publish(&self, snapshot: &Arc<DocumentSnapshot>, event: &impl Fn(Event)) {
        let native = self.native.as_deref().unwrap_or_default();
        let mut items = native.to_vec();
        if self.disk.as_deref() == Some(&snapshot.text) {
            items.extend(
                self.compiler
                    .iter()
                    .filter(|entry| {
                        !native.iter().any(|item| {
                            item["range"] == entry["range"] && item["code"] == entry["code"]
                        })
                    })
                    .map(|item| {
                        let mut item = item.clone();
                        if item["code"] == "E0601"
                            && let Some(entry) = &self.entry
                        {
                            item["range"] = entry["range"].clone();
                        }
                        item
                    }),
            );
        }
        if let Some(entry) = &self.entry
            && !items.iter().any(|item| item["code"] == "E0601")
        {
            items.push(entry.clone());
        }
        if self.native.is_none() && items.is_empty() {
            return;
        }
        if !self.members.is_empty() {
            let ranges: Vec<_> = items
                .iter()
                .filter(|item| {
                    item["code"] == "inactive-code"
                        || item["severity"] == 1
                        || item["severity"].is_null()
                })
                .filter_map(diagnostic_range)
                .collect();
            items.extend(
                self.members
                    .iter()
                    .filter(|member| {
                        diagnostic_range(member).is_some_and(|(start, end)| {
                            !ranges.iter().any(|&(from, to)| from < end && start < to)
                        })
                    })
                    .take(512usize.saturating_sub(items.len()))
                    .cloned(),
            );
        }
        event(Event::Diagnostics {
            snapshot: snapshot.clone(),
            items,
        });
    }
}

/// UTF-16 positions compare lexicographically across lines without rescanning the overlay.
fn diagnostic_range(item: &Value) -> Option<([u64; 2], [u64; 2])> {
    let position = |value: &Value| Some([value["line"].as_u64()?, value["character"].as_u64()?]);
    let range = &item["range"];
    Some((position(&range["start"])?, position(&range["end"])?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn native_errors_and_inactive_code_take_precedence_in_both_reply_orders() {
        let snapshot = Arc::new(DocumentSnapshot {
            opening: 1,
            revision: 1,
            path: None,
            text: String::new(),
        });
        let item = |line, severity, code| {
            json!({
                "range":{"start":{"line":line,"character":0},"end":{"line":line,"character":2}},
                "severity":severity,"code":code
            })
        };
        let members = vec![
            item(0, 1, "member"),
            item(1, 1, "member"),
            item(2, 1, "member"),
        ];
        let native = vec![item(0, 1, "E0425"), item(1, 4, "inactive-code")];
        for semantic_first in [true, false] {
            let mut open = Open {
                snapshot: snapshot.clone(),
                language: "rust",
                version: 1,
                save_generation: 0,
                diagnose_at: None,
                semantic_at: None,
                rust_diagnostics: Some(Default::default()),
            };
            let reports = RefCell::new(Vec::new());
            let publish = |event| {
                if let Event::Diagnostics { items, .. } = event {
                    reports.borrow_mut().push(items);
                }
            };
            if semantic_first {
                open.rust_diagnostics.as_mut().unwrap().update_members(
                    members.clone(),
                    &snapshot,
                    &publish,
                );
                assert!(reports.borrow().is_empty());
            }
            open.report_diagnostics(native.clone(), &publish);
            if !semantic_first {
                assert_eq!(reports.borrow().last(), Some(&native));
                open.rust_diagnostics.as_mut().unwrap().update_members(
                    members.clone(),
                    &snapshot,
                    &publish,
                );
            }
            assert_eq!(
                reports.borrow().last().unwrap(),
                &[native[0].clone(), native[1].clone(), members[2].clone()]
            );
        }
    }
}
