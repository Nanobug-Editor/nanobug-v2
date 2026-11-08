//! Route Explorer commands and reconcile open buffers after filesystem transfers.

use super::{Editor, buffer::Buffer};
use crate::explorer::{PathChange, TransferKind};
use gpui::{Context, Window};
use nanobug::{
    animation::scroll::{GridScroll, ScrollInput},
    editor::{Document, Vim},
};
use std::time::Duration;

impl Editor {
    /// Apply the paste operation identified by `key` and refresh the visible paths.
    pub(super) fn transfer(&mut self, key: &str) {
        let dirty: Vec<_> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.vim.doc.modified())
            .filter_map(|buffer| buffer.vim.doc.path.clone())
            .collect();
        let directory = self.tree.directory().to_path_buf();
        let result = if key == "v" {
            self.transfers.paste(&self.project, &directory, &dirty)
        } else {
            self.transfers.replay(key == "Z", &dirty)
        };
        self.status = match result {
            Ok(changes) => {
                self.apply_path_changes(&changes);
                if key == "v" && directory != self.project {
                    self.tree.expanded.insert(directory);
                }
                match key {
                    "v" => "已粘贴",
                    "z" => "已撤销粘贴",
                    _ => "已重做粘贴",
                }
                .into()
            }
            Err(error) => format!("{error:#}"),
        };
        if let Err(error) = self.tree.refresh() {
            self.status.push_str(&format!(" · 刷新失败：{error:#}"));
        }
    }

    /// Retarget buffers after moves in `changes` and drop stale views of replaced paths.
    fn apply_path_changes(&mut self, changes: &[PathChange]) {
        if changes.is_empty() {
            return;
        }
        let mut previous_index = 0;
        let mut active = self.active;
        self.buffers.retain_mut(|buffer| {
            let index = previous_index;
            previous_index += 1;
            if let Some((change, relative)) = buffer.vim.doc.path.as_ref().and_then(|path| {
                changes.iter().find_map(|change| {
                    path.strip_prefix(&change.from)
                        .ok()
                        .map(|relative| (change, relative))
                })
            }) {
                if let Some(to) = &change.to {
                    let path = if relative.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(relative)
                    };
                    buffer.vim.doc.path = Some(path);
                    return true;
                }
            } else if !buffer.vim.doc.path.as_ref().is_some_and(|path| {
                changes
                    .iter()
                    .any(|change| change.to.as_ref().is_some_and(|to| path.starts_with(to)))
            }) {
                return true;
            }
            active -= usize::from(index < self.active);
            false
        });
        if self.buffers.is_empty() {
            self.buffers.push(Buffer {
                vim: Vim::new(Document::new("")),
                scroll: GridScroll::default(),
                columns: GridScroll::default(),
            });
        }
        self.active = active.min(self.buffers.len() - 1);
        self.last_cursor_space = None;
    }

    /// Interpret a manager `key` after a possible `prefix`, returning its viewport intent.
    /// `window` and `cx` schedule a standalone undo when the second key of `z` does not arrive.
    pub(super) fn manager_key(
        &mut self,
        key: &str,
        prefix: Option<char>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ScrollInput> {
        let height = self.layout.tree_body().h;
        if prefix == Some('z') {
            let inset = match key {
                "t" => Some(0),
                "z" => Some(height.saturating_sub(1) / 2),
                "b" => Some(height.saturating_sub(1)),
                _ => None,
            };
            if let Some(inset) = inset {
                return Some(ScrollInput::Position(
                    self.tree.selected.saturating_sub(inset),
                ));
            }
            if key != "escape" {
                self.transfer("z");
            }
        }
        if let [digit @ b'0'..=b'9'] = key.as_bytes() {
            self.tree_count = (self.tree_count * 10 + usize::from(*digit - b'0')).min(9999);
            return Some(ScrollInput::Advance);
        }
        let count = std::mem::take(&mut self.tree_count).max(1);
        match key {
            "k" | "up" => self.tree.selected = self.tree.selected.saturating_sub(count),
            "j" | "down" => {
                self.tree.selected = self
                    .tree
                    .selected
                    .saturating_add(count)
                    .min(self.tree.entries.len().saturating_sub(1))
            }
            "home" => self.tree.selected = 0,
            "g" if prefix == Some('g') => self.tree.selected = 0,
            "g" => self.prefix = Some('g'),
            "G" | "end" => self.tree.selected = self.tree.entries.len().saturating_sub(1),
            "h" | "left" => self.tree.branch(false),
            "right" => self.tree.branch(true),
            "l" | "enter" => {
                if let Some(path) = self.tree.activate() {
                    self.open(path);
                }
            }
            "s" => self.status = format!("已标记 {} 项", self.tree.toggle_mark()),
            "escape" => {
                self.tree.marked.clear();
                self.status.clear();
            }
            "c" | "x" => {
                let kind = if key == "c" {
                    TransferKind::Copy
                } else {
                    TransferKind::Cut
                };
                self.status = match self.transfers.capture(&self.tree.marked, kind) {
                    Ok(count) => format!(
                        "已{} {count} 项，到目标目录按 v",
                        if key == "c" { "复制" } else { "剪切" }
                    ),
                    Err(error) => error.to_string(),
                };
            }
            "v" | "Z" => self.transfer(key),
            "z" => {
                self.prefix = Some('z');
                let delay = cx.background_executor().timer(Duration::from_millis(350));
                self.paste_timer = Some(cx.spawn_in(window, async move |weak, cx| {
                    delay.await;
                    let _ = cx.update(|window, cx| {
                        let _ = weak.update(cx, |editor, cx| {
                            if editor.prefix.take() == Some('z') {
                                editor.transfer("z");
                                editor.repaint([ScrollInput::Reveal; 3], window, cx);
                            }
                        });
                    });
                }));
            }
            _ if prefix == Some('z') => {}
            _ => return None,
        }
        Some(ScrollInput::Reveal)
    }
}
